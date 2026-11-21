//! Provenance for the private CLAP payload of this app build.
//!
//! Scanning can describe a bundle, but only the app's embedded manifest hash can authorize
//! in-process loading. The loader opens a private, verified copy, so replacing a file in the
//! installed distribution cannot change the bytes handed to the library loader.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};

const DIRECTORY: &str = "bundled-clap";
const MANIFEST: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 1_048_576;

/// Use the same canonical path spelling as the plugin scan cache.
fn canonical_app_path(path: &Path) -> io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    #[cfg(windows)]
    let canonical = crate::scan::without_verbatim(canonical);
    Ok(canonical)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    target: String,
    bundles: Vec<ManifestBundle>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestBundle {
    id: String,
    path: String,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    path: String,
    sha256: String,
}

#[derive(Debug)]
struct Bundle {
    id: String,
    files: BTreeMap<PathBuf, [u8; 32]>,
}

/// Why an installed bundled CLAP could not be verified for direct hosting.
#[derive(Debug)]
pub struct BundleVerificationError {
    path: PathBuf,
    reason: String,
}

impl BundleVerificationError {
    fn new(path: &Path, reason: impl std::fmt::Display) -> Self {
        Self {
            path: path.to_owned(),
            reason: reason.to_string(),
        }
    }
}

impl std::fmt::Display for BundleVerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bundled CLAP {}: {}", self.path.display(), self.reason)
    }
}

impl std::error::Error for BundleVerificationError {}

/// The app build's authenticated inventory of private CLAP bundles.
#[derive(Debug)]
pub struct BundledClap {
    root: PathBuf,
    cache_dir: PathBuf,
    bundles: BTreeMap<PathBuf, Bundle>,
    copies: RefCell<BTreeMap<PathBuf, VerifiedClap>>,
}

/// An in-process load target with bytes authenticated by the app's release manifest.
///
/// Keep this proof alive until the instance no longer needs its bundle files.
#[derive(Debug, Clone)]
pub struct VerifiedClap(Rc<VerifiedFiles>);

#[derive(Debug)]
struct VerifiedFiles {
    _directory: TempDir,
    snapshot: PathBuf,
    original: PathBuf,
    id: String,
}

impl VerifiedClap {
    /// The verified copy to pass to the in-process CLAP loader.
    pub fn bundle_path(&self) -> &Path {
        &self.0.snapshot
    }

    /// The installed bundle path used by the catalog and the isolated bridge path.
    pub fn original_path(&self) -> &Path {
        &self.0.original
    }

    /// The CLAP plugin ID authorized by the manifest for these bundle files.
    pub fn plugin_id(&self) -> &str {
        &self.0.id
    }
}

impl BundledClap {
    pub(crate) fn installed_directory() -> Result<Option<PathBuf>, BundleVerificationError> {
        if option_env!("KOMPONIST_BUNDLED_CLAP_MANIFEST_SHA256").is_none() {
            return Ok(None);
        }
        let executable = std::env::current_exe()
            .and_then(|path| canonical_app_path(&path))
            .map_err(|err| BundleVerificationError::new(Path::new("current executable"), err))?;
        root_for_executable(&executable, cfg!(target_os = "macos"))
            .map(Some)
            .ok_or_else(|| BundleVerificationError::new(&executable, "no app resource path"))
    }

    /// Discover the app's executable-relative CLAP inventory. Development builds without the
    /// embedded manifest digest do not have an inventory.
    ///
    /// `cache_dir` is an application-owned cache directory on an executable-capable volume.
    pub fn discover(cache_dir: &Path) -> Result<Option<Self>, BundleVerificationError> {
        let Some(embedded_hash) = option_env!("KOMPONIST_BUNDLED_CLAP_MANIFEST_SHA256") else {
            return Ok(None);
        };
        let Some(root) = Self::installed_directory()? else {
            return Ok(None);
        };
        Self::open(
            root,
            cache_dir.to_owned(),
            embedded_hash,
            env!("KOMPONIST_BUNDLED_CLAP_TARGET"),
        )
        .map(Some)
    }

    fn open(
        root: PathBuf,
        cache_dir: PathBuf,
        expected_digest: &str,
        target: &str,
    ) -> Result<Self, BundleVerificationError> {
        directory(&root)?;
        let canonical =
            canonical_app_path(&root).map_err(|err| BundleVerificationError::new(&root, err))?;
        if canonical != root {
            return Err(BundleVerificationError::new(
                &root,
                "private bundle directory resolves outside the app resource path",
            ));
        }

        let path = root.join(MANIFEST);
        let expected = parse_digest(expected_digest)
            .ok_or_else(|| BundleVerificationError::new(&path, "invalid embedded manifest hash"))?;
        let bytes = read_manifest(&path)?;
        if Sha256::digest(&bytes)[..] != expected {
            return Err(BundleVerificationError::new(
                &path,
                "manifest hash mismatch",
            ));
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|err| BundleVerificationError::new(&path, err))?;
        if manifest.schema_version != 1 || manifest.target != target {
            return Err(BundleVerificationError::new(
                &path,
                "unsupported schema version or target",
            ));
        }

        let mut ids = BTreeSet::new();
        let mut bundle_names = BTreeSet::new();
        let mut bundles = BTreeMap::new();
        for entry in manifest.bundles {
            let bundle_name = relative_path(&entry.path)
                .filter(|name| {
                    name.components().count() == 1
                        && name
                            .extension()
                            .is_some_and(|extension| extension == "clap")
                })
                .ok_or_else(|| BundleVerificationError::new(&path, "invalid bundle path"))?;
            if entry.id.is_empty()
                || !ids.insert(entry.id.clone())
                || !bundle_names.insert(entry.path.to_ascii_lowercase())
                || entry.files.is_empty()
            {
                return Err(BundleVerificationError::new(
                    &path,
                    "empty or duplicate bundle entry",
                ));
            }
            let mut files = BTreeMap::new();
            for file in entry.files {
                let relative = relative_path(&file.path)
                    .filter(|relative| relative.starts_with(&bundle_name))
                    .ok_or_else(|| {
                        BundleVerificationError::new(&path, "file path escapes its bundle")
                    })?;
                let digest = parse_digest(&file.sha256)
                    .ok_or_else(|| BundleVerificationError::new(&path, "invalid file hash"))?;
                if files.insert(relative, digest).is_some() {
                    return Err(BundleVerificationError::new(&path, "duplicate file path"));
                }
            }
            bundles.insert(
                root.join(bundle_name),
                Bundle {
                    id: entry.id,
                    files,
                },
            );
        }
        if bundles.is_empty() {
            return Err(BundleVerificationError::new(
                &path,
                "empty bundle inventory",
            ));
        }
        Ok(Self {
            root,
            cache_dir,
            bundles,
            copies: RefCell::new(BTreeMap::new()),
        })
    }

    /// Private app folder for the isolated discovery scanner.
    pub fn directory(&self) -> &Path {
        &self.root
    }

    /// Installed bundle paths and CLAP IDs from the verified manifest.
    pub fn bundle_entries(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.bundles
            .iter()
            .map(|(path, bundle)| (path.as_path(), bundle.id.as_str()))
    }

    /// Verify the exact installed path and identity before routing a load in-process.
    ///
    /// External paths return `None` even when their CLAP ID is the same. An invalid candidate
    /// from the private app directory returns an error, so the caller can surface the failure.
    pub fn verify(
        &self,
        path: &Path,
        plugin_id: &str,
    ) -> Result<Option<VerifiedClap>, BundleVerificationError> {
        let Some(bundle) = self.bundles.get(path) else {
            return if path.starts_with(&self.root) {
                Err(BundleVerificationError::new(
                    path,
                    "bundle absent from release inventory",
                ))
            } else {
                Ok(None)
            };
        };
        if bundle.id != plugin_id {
            return Err(BundleVerificationError::new(path, "plugin ID mismatch"));
        }
        directory(&self.root)?;
        let mut pending = vec![path.to_owned()];
        let mut files = BTreeSet::new();
        while let Some(item) = pending.pop() {
            let relative = item
                .strip_prefix(&self.root)
                .expect("item is under bundled root");
            let item_metadata = metadata(&item)?;
            if item_metadata.is_dir() {
                // Reject unlisted directory branches before opening them.
                if !bundle
                    .files
                    .keys()
                    .any(|file| file.starts_with(relative) && file != relative)
                {
                    return Err(BundleVerificationError::new(
                        &item,
                        "directory absent from inventory",
                    ));
                }
                let children =
                    fs::read_dir(&item).map_err(|err| BundleVerificationError::new(&item, err))?;
                for child in children {
                    let entry = child.map_err(|err| BundleVerificationError::new(&item, err))?;
                    let child = entry.path();
                    let relative = child
                        .strip_prefix(&self.root)
                        .expect("child is under bundled root");
                    if !bundle.files.keys().any(|file| file.starts_with(relative)) {
                        return Err(BundleVerificationError::new(
                            &child,
                            "entry absent from inventory",
                        ));
                    }
                    pending.push(child);
                }
            } else if !item_metadata.is_file() || !bundle.files.contains_key(relative) {
                return Err(BundleVerificationError::new(
                    &item,
                    "file absent from inventory",
                ));
            } else {
                files.insert(relative.to_owned());
            }
        }
        if files.len() != bundle.files.len() {
            return Err(BundleVerificationError::new(
                path,
                "bundle file inventory is incomplete",
            ));
        }

        if let Some(copy) = self.copies.borrow().get(path) {
            for (relative, hash) in &bundle.files {
                verify_file(&self.root.join(relative), hash, None)?;
            }
            return Ok(Some(copy.clone()));
        }

        let cache_dir = snapshot_cache(&self.cache_dir)?;
        let mut builder = Builder::new();
        builder.prefix("trusted-clap-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        let snapshot = builder
            .tempdir_in(&cache_dir)
            .map_err(|err| BundleVerificationError::new(&cache_dir, err))?;
        for (relative, hash) in &bundle.files {
            verify_file(
                &self.root.join(relative),
                hash,
                Some(&snapshot.path().join(relative)),
            )?;
        }
        let copy = VerifiedClap(Rc::new(VerifiedFiles {
            snapshot: snapshot
                .path()
                .join(path.file_name().expect("known bundle file name")),
            _directory: snapshot,
            original: path.to_owned(),
            id: bundle.id.clone(),
        }));
        self.copies
            .borrow_mut()
            .insert(path.to_owned(), copy.clone());
        Ok(Some(copy))
    }
}

pub(crate) fn snapshot_cache(path: &Path) -> Result<PathBuf, BundleVerificationError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| BundleVerificationError::new(path, error))?;
    directory(path)?;
    let root = fs::canonicalize(path).map_err(|error| BundleVerificationError::new(path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = rustix::process::geteuid().as_raw();
        let ancestors: Vec<_> = root.ancestors().collect();
        // Validate from the filesystem root down so each parent protects the next path entry.
        for ancestor in ancestors.into_iter().rev() {
            let info = metadata(ancestor)?;
            let owned_by_user = info.uid() == uid;
            let trusted_owner = owned_by_user || info.uid() == 0;
            let writable_by_others = info.mode() & 0o022 != 0;
            let sticky = info.mode() & 0o1000 != 0;
            let safe = if ancestor == root {
                owned_by_user && !writable_by_others
            } else {
                // A sticky shared directory such as /tmp still protects entries owned by
                // root or this user, which is checked on the next iteration.
                trusted_owner && (!writable_by_others || sticky)
            };
            if !safe {
                return Err(BundleVerificationError::new(
                    ancestor,
                    "unsafe owner or permissions for the CLAP snapshot cache",
                ));
            }
        }
    }
    Ok(root)
}

fn root_for_executable(executable: &Path, macos: bool) -> Option<PathBuf> {
    let parent = executable.parent()?;
    let resources = if macos {
        parent.parent()?.join("Resources")
    } else {
        parent.to_owned()
    };
    Some(resources.join(DIRECTORY))
}

fn relative_path(path: &str) -> Option<PathBuf> {
    // The release manifest uses forward slashes on every OS. Reject components that could
    // acquire a different meaning when this same manifest is interpreted on Windows.
    path.split('/')
        .all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', ':'])
                && !part.ends_with(['.', ' '])
                && !part.chars().any(char::is_control)
        })
        .then(|| PathBuf::from(path))
}

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut digest = [0; 32];
    for (byte, part) in digest.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        *byte = u8::from_str_radix(std::str::from_utf8(part).ok()?, 16).ok()?;
    }
    Some(digest)
}

fn metadata(path: &Path) -> Result<Metadata, BundleVerificationError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|err| BundleVerificationError::new(path, err))?;
    reject_links(path, &metadata)?;
    Ok(metadata)
}

fn reject_links(path: &Path, metadata: &Metadata) -> Result<(), BundleVerificationError> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(BundleVerificationError::new(
                path,
                "reparse point in bundled payload",
            ));
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(BundleVerificationError::new(
            path,
            "symlink in bundled payload",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_payload(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};

    // Check the opened handle as well: never follow a replacement symlink or block waiting
    // on a file replaced by a FIFO while inspecting the installation.
    open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

#[cfg(windows)]
fn open_payload(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    File::options()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_payload(path: &Path) -> io::Result<File> {
    File::open(path)
}

fn open_regular_payload(path: &Path) -> Result<(File, Metadata), BundleVerificationError> {
    let file = open_payload(path).map_err(|err| BundleVerificationError::new(path, err))?;
    let metadata = file
        .metadata()
        .map_err(|err| BundleVerificationError::new(path, err))?;
    reject_links(path, &metadata)?;
    if !metadata.is_file() {
        return Err(BundleVerificationError::new(path, "expected regular file"));
    }
    Ok((file, metadata))
}

fn directory(path: &Path) -> Result<(), BundleVerificationError> {
    if !metadata(path)?.is_dir() {
        return Err(BundleVerificationError::new(path, "expected directory"));
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<Vec<u8>, BundleVerificationError> {
    let (file, _) = open_regular_payload(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| BundleVerificationError::new(path, err))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(BundleVerificationError::new(
            path,
            "manifest size limit exceeded",
        ));
    }
    Ok(bytes)
}

fn verify_file(
    source: &Path,
    expected: &[u8; 32],
    destination: Option<&Path>,
) -> Result<(), BundleVerificationError> {
    let (mut source_file, _source_metadata) = open_regular_payload(source)?;
    let mut output = None;
    if let Some(path) = destination {
        let create = || -> io::Result<File> {
            fs::create_dir_all(path.parent().expect("snapshot file has a parent"))?;
            File::create_new(path)
        };
        output = Some(create().map_err(|err| BundleVerificationError::new(path, err))?);
    }
    let mut sha = Sha256::new();
    let mut buffer = [0; 32 * 1024];
    loop {
        let count = source_file
            .read(&mut buffer)
            .map_err(|err| BundleVerificationError::new(source, err))?;
        if count == 0 {
            break;
        }
        let bytes = &buffer[..count];
        sha.update(bytes);
        if let Some(output) = &mut output {
            output
                .write_all(bytes)
                .map_err(|err| BundleVerificationError::new(source, err))?;
        }
    }
    if sha.finalize()[..] != *expected {
        return Err(BundleVerificationError::new(
            source,
            "bundle file hash mismatch",
        ));
    }
    #[cfg(unix)]
    if let (Some(output), Some(destination)) = (output, destination) {
        // Copy normal permission bits so bundled resources retain executable access when needed.
        // The private snapshot never owns set-ID or sticky files.
        use std::os::unix::fs::PermissionsExt;
        let mode = _source_metadata.permissions().mode() & 0o777;
        output
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|err| BundleVerificationError::new(destination, err))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const TARGET: &str = "x86_64-unknown-linux-gnu";
    const ID: &str = "org.klang-studio.komponist.trust-test";
    const FILE: &str = "fixture.clap";
    const PAYLOAD: &[u8] = b"trusted fixture bytes";

    fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn manifest(files: &[(&str, &[u8])]) -> Value {
        let payloads: Vec<_> = files
            .iter()
            .map(|(path, bytes)| json!({"path": path, "sha256": digest(bytes)}))
            .collect();
        json!({
            "schema_version": 1,
            "target": TARGET,
            "bundles": [{"id": ID, "path": FILE, "files": payloads}],
        })
    }

    struct Fixture {
        base: TempDir,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let base = tempfile::tempdir().unwrap();
            let root = canonical_app_path(base.path()).unwrap().join(DIRECTORY);
            fs::create_dir(&root).unwrap();
            Self { base, root }
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }

        fn open(&self, manifest: &Value) -> Result<BundledClap, BundleVerificationError> {
            let bytes = serde_json::to_vec(manifest).unwrap();
            self.write(MANIFEST, &bytes);
            BundledClap::open(
                self.root.clone(),
                self.base.path().join("cache"),
                &digest(&bytes),
                TARGET,
            )
        }
    }

    #[test]
    fn anchors_manifest_to_the_executable_not_an_install_prefix() {
        let executable = Path::new("/moved/Komponist.app/Contents/MacOS/komponist");
        assert_eq!(
            root_for_executable(executable, true).unwrap(),
            Path::new("/moved/Komponist.app/Contents/Resources/bundled-clap")
        );
        let executable = Path::new("/moved/komponist/komponist");
        assert_eq!(
            root_for_executable(executable, false).unwrap(),
            Path::new("/moved/komponist/bundled-clap")
        );
    }

    #[test]
    fn loads_only_the_verified_snapshot_for_the_installed_path() {
        let fixture = Fixture::new();
        fixture.write(FILE, PAYLOAD);
        let inventory = fixture.open(&manifest(&[(FILE, PAYLOAD)])).unwrap();
        let installed = crate::scan::bundle_key(&fixture.root.join(FILE)).unwrap();
        assert_eq!(
            inventory.bundle_entries().collect::<Vec<_>>(),
            [(&*installed, ID)]
        );
        let external = fixture.base.path().join(FILE);
        fs::write(&external, PAYLOAD).unwrap();
        assert!(inventory.verify(&external, ID).unwrap().is_none());
        assert!(inventory.verify(&installed, "another-id").is_err());
        assert!(
            inventory
                .verify(&fixture.root.join("unknown.clap"), ID)
                .is_err()
        );

        let verified = inventory.verify(&installed, ID).unwrap().unwrap();
        assert_eq!(verified.original_path(), installed);
        assert_eq!(verified.plugin_id(), ID);
        assert_eq!(verified.bundle_path().file_name(), Some(FILE.as_ref()));
        assert_ne!(verified.bundle_path(), installed);
        assert_eq!(fs::read(verified.bundle_path()).unwrap(), PAYLOAD);
        assert_eq!(
            inventory
                .verify(&installed, ID)
                .unwrap()
                .unwrap()
                .bundle_path(),
            verified.bundle_path()
        );
        fixture.write(FILE, b"changed after stage");
        assert!(inventory.verify(&installed, ID).is_err());
        fs::remove_file(&installed).unwrap();
        assert!(inventory.verify(&installed, ID).is_err());
        drop(inventory);
        assert_eq!(fs::read(verified.bundle_path()).unwrap(), PAYLOAD);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_snapshot_cache_below_an_unprotected_shared_directory() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        fixture.write(FILE, PAYLOAD);
        let mut inventory = fixture.open(&manifest(&[(FILE, PAYLOAD)])).unwrap();
        let shared = fixture.base.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        inventory.cache_dir = shared.join("cache");

        let result = inventory.verify(&fixture.root.join(FILE), ID);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsafe owner or permissions")
        );
        assert_eq!(fs::read_dir(&inventory.cache_dir).unwrap().count(), 0);
    }

    #[test]
    fn requires_exact_directory_contents() {
        let fixture = Fixture::new();
        let executable = "fixture.clap/Contents/MacOS/Fixture";
        let plist = "fixture.clap/Contents/Info.plist";
        fixture.write(executable, PAYLOAD);
        fixture.write(plist, b"manifested aux file");
        let inventory = fixture
            .open(&manifest(&[
                (executable, PAYLOAD),
                (plist, b"manifested aux file"),
            ]))
            .unwrap();
        let path = fixture.root.join(FILE);
        let proof = inventory.verify(&path, ID).unwrap().unwrap();
        assert_eq!(
            fs::read(proof.bundle_path().join("Contents/MacOS/Fixture")).unwrap(),
            PAYLOAD
        );
        fixture.write("fixture.clap/Contents/unlisted", b"extra file");
        assert!(inventory.verify(&path, ID).is_err());
        fs::remove_file(fixture.root.join("fixture.clap/Contents/unlisted")).unwrap();
        fs::remove_file(fixture.root.join(plist)).unwrap();
        assert!(inventory.verify(&path, ID).is_err());
    }

    #[test]
    fn rejects_unanchored_manifest_wrong_target_and_invalid_entries() {
        let fixture = Fixture::new();
        let mut valid = manifest(&[(FILE, PAYLOAD)]);
        fixture.write(MANIFEST, &serde_json::to_vec(&valid).unwrap());
        assert!(
            BundledClap::open(
                fixture.root.clone(),
                fixture.root.clone(),
                &digest(b"different"),
                TARGET
            )
            .unwrap_err()
            .to_string()
            .contains("manifest hash mismatch")
        );
        valid["target"] = json!("aarch64-apple-darwin");
        assert!(fixture.open(&valid).is_err());

        let mut duplicate = manifest(&[(FILE, PAYLOAD)]);
        let entry = duplicate["bundles"][0].clone();
        duplicate["bundles"].as_array_mut().unwrap().push(entry);
        assert!(fixture.open(&duplicate).is_err());
        for path in [
            "../fixture.clap",
            "/fixture.clap",
            "fixture.clap/../escape",
            "C:fixture.clap",
        ] {
            assert!(
                fixture.open(&manifest(&[(path, PAYLOAD)])).is_err(),
                "{path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_even_when_the_destination_has_matching_bytes() {
        let fixture = Fixture::new();
        let external = fixture.base.path().join("target");
        fs::write(&external, PAYLOAD).unwrap();
        let path = fixture.root.join(FILE);
        std::os::unix::fs::symlink(external, &path).unwrap();
        let inventory = fixture.open(&manifest(&[(FILE, PAYLOAD)])).unwrap();
        assert!(
            inventory
                .verify(&path, ID)
                .unwrap_err()
                .to_string()
                .contains("symlink")
        );
    }
}
