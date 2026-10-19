//! Workspace filesystem sandbox.

use crate::{Error, Result};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub struct WorkspaceFs {
    root: PathBuf,
    extra_roots: Vec<PathBuf>,
}

impl WorkspaceFs {
    pub fn new(root: PathBuf) -> Result<Self> {
        Self::with_extra_roots(root, &[])
    }

    /// Primary `root` is cwd / relative-path base. Extra roots are additional
    /// allowlist directories (CD3). Per-dir read-only is a V1 non-goal.
    pub fn with_extra_roots(root: PathBuf, extra: &[PathBuf]) -> Result<Self> {
        let root = canonicalize_existing_dir(&root)?;
        let mut extra_roots = Vec::new();
        for raw in extra {
            match canonicalize_existing_dir(raw) {
                Ok(canon) => {
                    if is_denied_sandbox_root(&canon) {
                        tracing::warn!(
                            path = %canon.display(),
                            "sandbox extra root skipped: system or home"
                        );
                        continue;
                    }
                    if path_is_inside(&root, &canon) || extra_roots.iter().any(|e| e == &canon) {
                        continue;
                    }
                    extra_roots.push(canon);
                }
                Err(err) => {
                    tracing::warn!(path = %raw.display(), %err, "sandbox extra root skipped");
                }
            }
        }
        Ok(Self { root, extra_roots })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn extra_roots(&self) -> &[PathBuf] {
        &self.extra_roots
    }

    fn allows(&self, resolved: &Path) -> bool {
        path_is_inside(&self.root, resolved)
            || self
                .extra_roots
                .iter()
                .any(|root| path_is_inside(root, resolved))
    }

    pub fn resolve(&self, user_path: &str) -> Result<PathBuf> {
        let resolved = self.candidate_path(user_path)?;
        if !self.allows(&resolved) {
            return Err(Error::Invalid(format!(
                "path escapes workspace: {user_path}"
            )));
        }
        Ok(resolved)
    }

    /// Resolve and reject protected system paths, but leave the workspace
    /// boundary check to the caller so an interactive host can ask for access.
    pub fn candidate_path(&self, user_path: &str) -> Result<PathBuf> {
        if user_path.trim().is_empty() {
            return Err(Error::Invalid("path must not be empty".into()));
        }
        if is_system_path(Path::new(user_path)) {
            return Err(Error::Invalid(format!("system path denied: {user_path}")));
        }
        let joined = {
            let p = Path::new(user_path);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.root.join(p)
            }
        };
        let normalized = lexical_normalize(&joined);
        if normalized.as_os_str().is_empty() {
            return Err(Error::Invalid("path must not be empty".into()));
        }

        let resolved = if normalized.exists() {
            dunce::canonicalize(&normalized).map_err(|err| {
                Error::Invalid(format!("canonicalize {}: {err}", normalized.display()))
            })?
        } else {
            resolve_via_parent(&normalized)?
        };

        if is_system_path(&resolved) || is_system_path(&normalized) {
            return Err(Error::Invalid(format!(
                "system path denied: {}",
                resolved.display()
            )));
        }

        Ok(resolved)
    }

    pub fn display_path(&self, path: &Path) -> String {
        let child = dunce::simplified(path);
        for root in std::iter::once(&self.root).chain(self.extra_roots.iter()) {
            let root = dunce::simplified(root);
            if let Ok(rel) = child.strip_prefix(root.as_ref() as &Path) {
                return rel.to_string_lossy().into_owned();
            }
        }
        path.to_string_lossy().into_owned()
    }
}

pub fn sandbox_error_for_model(err: &Error) -> String {
    let msg = err.to_string();
    if msg.contains("escapes workspace") {
        if let Some((_, path)) = msg.split_once(": ") {
            return format!("Error: path escapes workspace: {path}");
        }
        return format!("Error: path escapes workspace: {msg}");
    }
    if msg.contains("system path denied") {
        if let Some((_, path)) = msg.split_once(": ") {
            return format!("Error: system path denied: {path}");
        }
        return format!("Error: system path denied: {msg}");
    }
    format!("Error: {msg}")
}

fn resolve_via_parent(normalized: &Path) -> Result<PathBuf> {
    let mut parent = normalized.parent();
    while let Some(p) = parent {
        if p.exists() {
            let canon_parent = dunce::canonicalize(p).map_err(|err| {
                Error::Invalid(format!("canonicalize parent {}: {err}", p.display()))
            })?;
            let rest = normalized.strip_prefix(p).unwrap_or(normalized);
            return Ok(canon_parent.join(rest));
        }
        parent = p.parent();
    }
    Err(Error::Invalid(format!(
        "path has no existing parent: {}",
        normalized.display()
    )))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

pub fn path_is_inside(root: &Path, candidate: &Path) -> bool {
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let child = if candidate.exists() {
        dunce::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf())
    } else {
        resolve_via_parent(&lexical_normalize(candidate))
            .unwrap_or_else(|_| candidate.to_path_buf())
    };
    let root = dunce::simplified(&root);
    let child = dunce::simplified(&child);
    child.starts_with(root.as_ref() as &Path)
}

pub fn is_system_path(path: &Path) -> bool {
    let simplified = dunce::simplified(path);
    let s = simplified.to_string_lossy().replace('/', "\\");
    let lower = s.to_lowercase();
    let mut prefixes = vec![
        r"c:\windows".to_string(),
        r"c:\program files".to_string(),
        r"c:\program files (x86)".to_string(),
    ];
    if let Ok(windir) = std::env::var("WINDIR") {
        prefixes.push(windir.replace('/', "\\").to_lowercase());
    }
    if prefixes.into_iter().any(|pre| {
        let pre = pre.trim_end_matches('\\').to_string();
        lower == pre || lower.starts_with(&format!("{pre}\\"))
    }) {
        return true;
    }
    unix_system_path(path)
}

fn unix_system_path(path: &Path) -> bool {
    let s = path.to_string_lossy().replace('\\', "/").to_lowercase();
    const PREFIXES: &[&str] = &[
        "/etc", "/usr", "/bin", "/sbin", "/sys", "/proc", "/dev", "/boot",
    ];
    PREFIXES
        .iter()
        .any(|pre| s == *pre || s.starts_with(&format!("{pre}/")))
}

fn is_drive_root(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
}

fn is_home_root(path: &Path) -> bool {
    let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) else {
        return false;
    };
    let Ok(home) = dunce::canonicalize(&home) else {
        return false;
    };
    dunce::simplified(path).as_os_str() == dunce::simplified(&home).as_os_str()
}

pub fn is_denied_sandbox_root(path: &Path) -> bool {
    is_system_path(path) || is_home_root(path) || is_drive_root(path)
}

fn canonicalize_existing_dir(root: &Path) -> Result<PathBuf> {
    if !root.exists() || !root.is_dir() {
        return Err(Error::Invalid(format!(
            "workspace must be an existing directory: {}",
            root.display()
        )));
    }
    dunce::canonicalize(root).map_err(|err| {
        Error::Invalid(format!(
            "workspace canonicalize failed ({}): {err}",
            root.display()
        ))
    })
}

/// Canonicalize a candidate 06 whitelist root. System paths, the user home
/// directory itself, and drive roots are hard-denied (not a Settings toggle).
pub fn validate_sandbox_root(path: &Path) -> Result<PathBuf> {
    if is_system_path(path) {
        return Err(Error::Invalid(format!(
            "system path denied: {}",
            path.display()
        )));
    }
    let canon = canonicalize_existing_dir(path)?;
    if is_system_path(&canon) {
        return Err(Error::Invalid(format!(
            "system path denied: {}",
            canon.display()
        )));
    }
    if is_home_root(&canon) {
        return Err(Error::Invalid(format!(
            "home directory denied: {}",
            canon.display()
        )));
    }
    if is_drive_root(&canon) {
        return Err(Error::Invalid(format!(
            "drive root denied: {}",
            canon.display()
        )));
    }
    Ok(canon)
}

pub fn command_targets_system_path(command: &str) -> bool {
    let lower = command.replace('/', "\\").to_lowercase();
    lower.contains(r"c:\windows")
        || lower.contains(r"c:\program files")
        || lower.contains("%windir%")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn relative_path_inside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let p = fs.resolve("a.txt").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "hi");
    }

    #[test]
    fn parent_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let err = fs.resolve("../secret").unwrap_err();
        assert!(err.to_string().contains("escapes workspace"), "{err}");
    }

    #[test]
    fn windows_system_path_denied() {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let err = fs
            .resolve(r"C:\Windows\System32\drivers\etc\hosts")
            .unwrap_err();
        assert!(err.to_string().contains("system path denied"), "{err}");
    }

    #[test]
    fn missing_file_uses_parent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("sub");
        fs::create_dir(&nested).unwrap();
        let ws = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let p = ws.resolve("sub/new.txt").unwrap();
        assert!(p.ends_with("new.txt"));
        assert!(p.starts_with(dunce::canonicalize(dir.path()).unwrap()));
    }

    #[test]
    fn empty_path_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        assert!(fs.resolve("  ").is_err());
    }

    #[test]
    fn path_is_inside_rejects_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dunce::canonicalize(dir.path()).unwrap().join("a.txt");
        assert!(path_is_inside(dir.path(), dir.path()));
        assert!(path_is_inside(dir.path(), &inside));
        let sibling = dir.path().parent().unwrap().join("other");
        assert!(!path_is_inside(dir.path(), &sibling));
    }

    #[test]
    fn extra_root_allows_absolute_read() {
        let primary = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        fs::write(primary.path().join("a.txt"), "A").unwrap();
        fs::write(extra.path().join("b.txt"), "B").unwrap();
        let fs = WorkspaceFs::with_extra_roots(
            primary.path().to_path_buf(),
            &[extra.path().to_path_buf()],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(fs.resolve("a.txt").unwrap()).unwrap(),
            "A"
        );
        let extra_file = dunce::canonicalize(extra.path()).unwrap().join("b.txt");
        assert_eq!(
            fs::read_to_string(fs.resolve(extra_file.to_str().unwrap()).unwrap()).unwrap(),
            "B"
        );
    }

    #[test]
    fn extra_root_does_not_allow_unlisted_sibling() {
        let primary = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("secret.txt"), "nope").unwrap();
        let fs = WorkspaceFs::with_extra_roots(
            primary.path().to_path_buf(),
            &[extra.path().to_path_buf()],
        )
        .unwrap();
        let secret = dunce::canonicalize(other.path())
            .unwrap()
            .join("secret.txt");
        let err = fs.resolve(secret.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("escapes workspace"), "{err}");
    }

    #[test]
    fn candidate_path_exposes_external_scope_without_granting_it() {
        let primary = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(primary.path().to_path_buf()).unwrap();
        let requested = external.path().join("new").join("report.html");
        let candidate = fs.candidate_path(requested.to_str().unwrap()).unwrap();
        let canonical_request = dunce::canonicalize(external.path())
            .unwrap()
            .join("new")
            .join("report.html");
        assert_eq!(candidate, canonical_request);
        assert!(fs.resolve(requested.to_str().unwrap()).is_err());
        let approved = WorkspaceFs::with_extra_roots(
            primary.path().to_path_buf(),
            &[external.path().to_path_buf()],
        )
        .unwrap();
        assert_eq!(
            approved.resolve(requested.to_str().unwrap()).unwrap(),
            canonical_request
        );
    }

    #[test]
    fn validate_rejects_windows_system_root() {
        let err = validate_sandbox_root(Path::new(r"C:\Windows")).unwrap_err();
        assert!(err.to_string().contains("system path denied"), "{err}");
    }
}
