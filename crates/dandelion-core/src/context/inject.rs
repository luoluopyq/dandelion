use super::discover::{render_project_segment, ContextKind, ProjectBundle};
use crate::config::Settings;
use crate::sandbox::WorkspaceFs;
use std::collections::HashSet;
use std::path::Path;

/// Session-scoped progressive discovery. Held under `Mutex` because dispatch is concurrent.
pub struct ContextSession {
    kind: Option<ContextKind>,
    injected: HashSet<String>,
    remaining: usize,
}

impl ContextSession {
    pub fn from_bundle(bundle: &ProjectBundle, settings: &Settings) -> Self {
        Self {
            kind: bundle.kind,
            injected: bundle.files.iter().map(|f| f.rel.clone()).collect(),
            remaining: settings
                .context_max_inject_bytes
                .saturating_sub(bundle.used_bytes),
        }
    }

    pub fn none() -> Self {
        Self {
            kind: None,
            injected: HashSet::new(),
            remaining: 0,
        }
    }

    pub fn take_for_dir(&mut self, fs: &WorkspaceFs, dir: &Path, settings: &Settings) -> String {
        let Some(kind) = self.kind else {
            return String::new();
        };
        if self.remaining == 0 {
            return String::new();
        }
        let files = super::discover::discover_from_dir(
            fs,
            kind,
            dir,
            &self.injected,
            self.remaining,
            settings,
        );
        if files.is_empty() {
            return String::new();
        }
        let used: usize = files
            .iter()
            .map(|f| format!("## {}\n\n{}\n\n", f.header_name, f.body.trim_end()).len())
            .sum();
        for f in &files {
            self.injected.insert(f.rel.clone());
        }
        self.remaining = self.remaining.saturating_sub(used);
        render_project_segment(&files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::discover::{discover_session_start, render_project_segment};
    use crate::sandbox::WorkspaceFs;
    use std::fs;

    #[test]
    fn none_never_discovers() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("pkg")).unwrap();
        fs::write(tmp.path().join("pkg/.dandelion.md"), "NESTED\n").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let mut s = Settings::defaults();
        s.context_enabled = true;
        let mut sess = ContextSession::none();
        let extra = sess.take_for_dir(&fs, &tmp.path().join("pkg"), &s);
        assert!(extra.is_empty());
    }

    #[test]
    fn progressive_appends_once() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".dandelion.md"), "ROOT_NOTE\n").unwrap();
        let pkg = tmp.path().join("pkg");
        fs::create_dir(&pkg).unwrap();
        fs::write(pkg.join(".dandelion.md"), "NESTED_UNIQUE_CTX\n").unwrap();
        let mut s = Settings::defaults();
        s.context_enabled = true;
        s.context_max_inject_bytes = 32_768;
        s.context_max_file_bytes = 65_536;
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let bundle = discover_session_start(&fs, &s);
        assert!(render_project_segment(&bundle.files).contains("ROOT_NOTE"));
        assert!(!render_project_segment(&bundle.files).contains("NESTED_UNIQUE_CTX"));
        let mut sess = ContextSession::from_bundle(&bundle, &s);
        let first = sess.take_for_dir(&fs, &pkg, &s);
        assert!(first.contains("NESTED_UNIQUE_CTX"), "{first}");
        let second = sess.take_for_dir(&fs, &pkg, &s);
        assert!(second.is_empty(), "same path must be once per session");
    }
}
