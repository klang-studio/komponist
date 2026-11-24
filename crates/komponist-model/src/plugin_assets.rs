//! Relocatable host-owned file links for plugin asset slots.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetLink {
    #[serde(with = "crate::audio::path_text")]
    pub abs: PathBuf,
    #[serde(
        default,
        with = "crate::audio::path_text",
        skip_serializing_if = "crate::audio::path_text::is_empty"
    )]
    pub rel: PathBuf,
    /// Optional display label retained when a file moves or an older link is migrated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl AssetLink {
    pub fn new(path: &Path, project_dir: Option<&Path>) -> Self {
        let abs = paths::absolute(path);
        let rel = project_dir
            .map(|dir| paths::relative(&abs, &paths::absolute(dir)))
            .filter(|path| path.is_relative())
            .unwrap_or_default();
        Self {
            abs,
            rel,
            display_name: None,
        }
    }

    /// The display name or the linked file's stem.
    pub fn name(&self) -> Option<Cow<'_, str>> {
        self.display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .map(Cow::Borrowed)
            .or_else(|| {
                self.abs
                    .file_stem()
                    .or_else(|| self.rel.file_stem())
                    .map(|stem| stem.to_string_lossy())
            })
    }

    /// Rebase a link while keeping its original display name if the file was renamed.
    pub fn moved_to(&self, path: &Path, project_dir: Option<&Path>) -> Self {
        let mut link = Self::new(path, project_dir);
        link.display_name.clone_from(&self.display_name);
        if link.display_name.is_none() && self.name() != link.name() {
            link.display_name = self.name().map(Cow::into_owned);
        }
        link
    }

    fn relative_candidate(&self, project_dir: Option<&Path>) -> Option<PathBuf> {
        let dir = project_dir?;
        if self.rel.as_os_str().is_empty() || self.rel.is_absolute() {
            return None;
        }
        let path = paths::normalize(&paths::absolute(dir).join(&self.rel));
        path.is_absolute().then_some(path)
    }

    /// Finds a linked file after moving the project, then tries the stored absolute path.
    pub fn resolve(&self, project_dir: Option<&Path>) -> Option<PathBuf> {
        self.relative_candidate(project_dir)
            .filter(|path| path.is_file())
            .or_else(|| (self.abs.is_absolute() && self.abs.is_file()).then(|| self.abs.clone()))
    }

    /// Returns the resolved file or the intended absolute location for a missing file.
    pub fn runtime_path(&self, project_dir: Option<&Path>) -> Option<PathBuf> {
        self.resolve(project_dir)
            .or_else(|| self.relative_candidate(project_dir))
            .or_else(|| self.abs.is_absolute().then(|| self.abs.clone()))
    }
}
