//! Finds catalog keys that no source file refers to (#245).
//!
//! A key counts as used when its accessor (`dock_devices`), its constant (`DOCK_DEVICES`) or the key
//! itself (`"dock-devices"`, as in undo labels and `Lang::by_id`) appears as a whole word in any `.rs`
//! file of the workspace outside the catalogs: sources, tests, `builtin-devices/` and the other crates.
//! Lines that only hold a comment do not count. A key that only tests use counts as used.
//!
//! Keys that are looked up by a computed name never show up as a word. Such a prefix goes into
//! [`DYNAMIC_PREFIXES`] with the reason. No lookup in the workspace needs one today: undo labels are
//! literal ids, device and parameter texts go through accessors.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Prefixes of keys that are looked up by a computed name, each with the reason (kept out of the
/// "unused" list). Empty today; an entry that matches no key fails the test, so it cannot go stale.
const DYNAMIC_PREFIXES: &[(&str, &str)] = &[];

/// Folders that hold no workspace source: build output, VCS data, the catalogs themselves and the
/// fixture catalog of the generator tests.
const SKIP_DIRS: &[&str] = &["target", ".git", "locales", "fixtures", "node_modules"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/komponist-i18n has the workspace root two levels up")
        .to_path_buf()
}

/// Collects the `.rs` files below `dir`; symlinks are not followed (a worktree's `target` is one).
/// A folder or entry that cannot be read fails the test: an incomplete scan would report live keys as
/// unused.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        let path = entry.path();
        let kind = entry
            .file_type()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let name = entry.file_name();
        if kind.is_dir() {
            if !SKIP_DIRS.iter().any(|s| name == *s) {
                rust_files(&path, out);
            }
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every word (`[A-Za-z0-9_-]+`) on a line that is not a comment line, over the whole workspace.
/// The tests of this crate are skipped: they name keys on purpose.
fn source_words(root: &Path) -> BTreeSet<String> {
    let own_tests = root.join("crates/komponist-i18n/tests");
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    let mut words = BTreeSet::new();
    for file in files.iter().filter(|f| !f.starts_with(&own_tests)) {
        let text =
            std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            words.extend(
                line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                    .filter(|w| !w.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    words
}

/// The message ids of the German catalog (the source language) as `(file, key)`.
fn catalog_keys(root: &Path) -> Vec<(String, String)> {
    let dir = root.join("crates/komponist-i18n/locales/de");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| {
            e.unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .path()
        })
        .filter(|p| p.extension().is_some_and(|e| e == "ftl"))
        .collect();
    paths.sort();
    let mut keys = Vec::new();
    for path in paths {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for line in text.lines() {
            // A message starts in column 0 with a lower-case id; terms (`-name`), comments and
            // continuation lines do not.
            if !line.starts_with(|c: char| c.is_ascii_lowercase()) {
                continue;
            }
            if let Some((id, _)) = line.split_once('=') {
                keys.push((file.clone(), id.trim().to_owned()));
            }
        }
    }
    keys
}

#[test]
fn every_catalog_key_is_used() {
    let root = workspace_root();
    let words = source_words(&root);
    let keys = catalog_keys(&root);
    assert!(keys.len() > 1000, "found only {} catalog keys", keys.len());

    for (prefix, reason) in DYNAMIC_PREFIXES {
        assert!(
            keys.iter().any(|(_, k)| k.starts_with(prefix)),
            "DYNAMIC_PREFIXES entry \"{prefix}\" ({reason}) matches no key: remove it"
        );
    }

    let unused: Vec<String> = keys
        .iter()
        .filter(|(_, key)| {
            let method = key.replace('-', "_");
            !(words.contains(key.as_str())
                || words.contains(&method)
                || words.contains(&method.to_ascii_uppercase()))
        })
        .filter(|(_, key)| !DYNAMIC_PREFIXES.iter().any(|(p, _)| key.starts_with(p)))
        .map(|(file, key)| format!("  {file}: {key}"))
        .collect();
    assert!(
        unused.is_empty(),
        "{} catalog key(s) are not used anywhere in the workspace. Delete them in locales/de and \
         locales/en, or add a prefix to DYNAMIC_PREFIXES if they are looked up by a computed name:\n{}",
        unused.len(),
        unused.join("\n")
    );
}
