//! Project files `*.komponist` (ARCHITECTURE §5.5, decision D7).
//!
//! One JSON file: `{ "format": "komponist", "version": 1, "project": { … } }`. Plugin states are
//! base64 strings. A missing plugin keeps its state byte-identical through load and save (S06).
//! Older versions are upgraded on load by a chain of `migrate_vN_to_vN+1` functions on the JSON
//! value, one per version step, each with its own test.
//!
//! Thread: any non-RT thread (loading and saving run on workers).

use std::fmt;

use crate::project::Project;

/// Value of the `format` field.
pub const FORMAT_TAG: &str = "komponist";

/// Version written by this build; files with a higher version are rejected.
pub const FORMAT_VERSION: u32 = 1;

/// File name extension, without the dot.
pub const FILE_EXTENSION: &str = "komponist";

/// Why a project could not be loaded or saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistError {
    /// Not JSON, or JSON that does not match the schema; the message comes from serde.
    Malformed(String),
    /// The `format` field is missing or not [`FORMAT_TAG`].
    NotAProject,
    /// The file was written by a newer komponist.
    TooNew {
        /// Version in the file.
        found: u32,
    },
    /// A migration step failed.
    Migration {
        /// Version the failed step started from.
        from: u32,
        /// What went wrong.
        message: String,
    },
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PersistError::Malformed(m) => write!(f, "malformed project file: {m}"),
            PersistError::NotAProject => write!(f, "not a komponist project file"),
            PersistError::TooNew { found } => {
                write!(
                    f,
                    "project file version {found} is newer than {FORMAT_VERSION}"
                )
            }
            PersistError::Migration { from, message } => {
                write!(f, "migration from version {from} failed: {message}")
            }
        }
    }
}

impl std::error::Error for PersistError {}

/// Serialises `project` into the file format (pretty-printed JSON, stable field order).
pub fn to_json(project: &Project) -> Result<String, PersistError> {
    todo!("WP-12")
}

/// Parses a project file of any supported version, migrating it to the current schema.
pub fn from_json(text: &str) -> Result<Project, PersistError> {
    todo!("WP-12")
}

/// Upgrades the `project` JSON value of a file with version `from` to [`FORMAT_VERSION`] by
/// running the migration steps in order. Identity for `from == FORMAT_VERSION`.
pub fn migrate(project: serde_json::Value, from: u32) -> Result<serde_json::Value, PersistError> {
    todo!("WP-12")
}

/// serde helper for [`crate::Device::state`]: `Option<Arc<[u8]>>` as an optional standard base64
/// string (with padding). `None` is written as `null`; a missing field reads as `None`.
pub mod base64_state {
    use std::sync::Arc;

    use serde::{Deserializer, Serializer};

    /// Writes the state as base64 or `null`.
    pub fn serialize<S: Serializer>(
        state: &Option<Arc<[u8]>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        todo!("WP-12")
    }

    /// Reads a base64 string or `null`.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<[u8]>>, D::Error> {
        todo!("WP-12")
    }
}
