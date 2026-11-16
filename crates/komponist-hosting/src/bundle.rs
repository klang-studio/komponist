//! Plugin bundles on macOS: a `CFBundle` of a `.clap` / `.vst3` directory and its executable.
//!
//! On macOS both formats ship as bundles (`X.vst3/Contents/MacOS/X`, `X.clap/Contents/MacOS/X`,
//! the executable's name given by `CFBundleExecutable` in `Contents/Info.plist`). The VST3 module
//! spec wants `bundleEntry(CFBundleRef)` instead of Linux's `ModuleEntry(handle)`, and the bundle
//! must stay alive until `bundleExit()`; CLAP (through clack-host) resolves the executable itself,
//! and the hosting layer needs the same path to keep the library mapped (`crate::library`).
//!
//! Raw CoreFoundation FFI (the framework is always there on macOS), no extra crate.

use std::ffi::c_void;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFURLRef = *const c_void;
type CFBundleRef = *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFAllocatorRef,
        buffer: *const u8,
        length: isize,
        is_directory: u8,
    ) -> CFURLRef;
    fn CFURLGetFileSystemRepresentation(
        url: CFURLRef,
        resolve_against_base: u8,
        buffer: *mut u8,
        max_length: isize,
    ) -> u8;
    fn CFBundleCreate(allocator: CFAllocatorRef, url: CFURLRef) -> CFBundleRef;
    fn CFBundleCopyExecutableURL(bundle: CFBundleRef) -> CFURLRef;
}

/// Longest path `CFURLGetFileSystemRepresentation` writes (`PATH_MAX`, with the NUL).
const PATH_MAX: usize = 1024;

/// An owned `CFBundleRef` (released on drop).
#[derive(Debug)]
pub(crate) struct CfBundle(CFBundleRef);

// SAFETY: CoreFoundation's retain/release are thread-safe, and the bundle is only read after
// creation (its executable URL; the plugin's `bundleEntry` gets the pointer).
unsafe impl Send for CfBundle {}
// SAFETY: as above.
unsafe impl Sync for CfBundle {}

impl CfBundle {
    /// The bundle at directory `path`, or `None` if `path` is no bundle directory.
    pub(crate) fn open(path: &Path) -> Option<CfBundle> {
        if !path.is_dir() {
            return None;
        }
        let bytes = path.as_os_str().as_bytes();
        // SAFETY: `bytes` is valid for its length; the URL is released below.
        let url = unsafe {
            CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                1,
            )
        };
        if url.is_null() {
            return None;
        }
        // SAFETY: `url` is a live CFURL; CFBundleCreate retains what it needs.
        let bundle = unsafe { CFBundleCreate(std::ptr::null(), url) };
        // SAFETY: created above, released once.
        unsafe { CFRelease(url) };
        (!bundle.is_null()).then_some(CfBundle(bundle))
    }

    /// The `CFBundleRef`, valid while `self` lives.
    pub(crate) fn as_ptr(&self) -> *mut c_void {
        self.0
    }

    /// The path of the bundle's executable (`Contents/MacOS/<CFBundleExecutable>`), if it has
    /// one that exists.
    pub(crate) fn executable(&self) -> Option<PathBuf> {
        // SAFETY: the bundle is live; the copied URL is released below.
        let url = unsafe { CFBundleCopyExecutableURL(self.0) };
        if url.is_null() {
            return None;
        }
        let mut buffer = [0u8; PATH_MAX];
        // SAFETY: `buffer` has room for `PATH_MAX` bytes including the NUL.
        let ok = unsafe {
            CFURLGetFileSystemRepresentation(url, 1, buffer.as_mut_ptr(), PATH_MAX as isize)
        };
        // SAFETY: copied above, released once.
        unsafe { CFRelease(url) };
        if ok == 0 {
            return None;
        }
        let len = buffer.iter().position(|&b| b == 0)?;
        let path = PathBuf::from(std::ffi::OsString::from_vec(buffer[..len].to_vec()));
        path.is_file().then_some(path)
    }
}

impl Drop for CfBundle {
    fn drop(&mut self) {
        // SAFETY: created by CFBundleCreate, released once.
        unsafe { CFRelease(self.0) };
    }
}

/// The executable of a bundle directory: the `CFBundleExecutable` of its `Info.plist`, else
/// `Contents/MacOS/<bundle name without extension>`. `None` for a path that is no directory.
pub(crate) fn executable(bundle: &Path) -> Option<PathBuf> {
    if let Some(path) = CfBundle::open(bundle).and_then(|b| b.executable()) {
        return Some(path);
    }
    let fallback = bundle.join("Contents/MacOS").join(bundle.file_stem()?);
    fallback.is_file().then_some(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kp-bundle-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_info_plist_names_the_executable() {
        let dir = temp_dir("plist");
        // Without Info.plist: the bundle's own name. (CoreFoundation caches a bundle per URL with
        // what it read, so each case gets a bundle of its own.)
        let plain = dir.join("Odd Name.vst3");
        let plain_macos = plain.join("Contents/MacOS");
        std::fs::create_dir_all(&plain_macos).unwrap();
        assert_eq!(executable(&plain), None);
        std::fs::write(plain_macos.join("Odd Name"), b"").unwrap();
        assert_eq!(executable(&plain), Some(plain_macos.join("Odd Name")));
        // With one: its CFBundleExecutable wins.
        let bundle = dir.join("Listed.vst3");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("Listed"), b"").unwrap();
        std::fs::write(macos.join("Real"), b"").unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>Real</string>
<key>CFBundleIdentifier</key><string>de.klang-studio.kp-bundle-test</string>
</dict></plist>"#,
        )
        .unwrap();
        let found = executable(&bundle).unwrap();
        assert_eq!(found.file_name().unwrap(), "Real");
        assert_eq!(
            found.canonicalize().unwrap(),
            macos.join("Real").canonicalize().unwrap()
        );
        assert!(CfBundle::open(&bundle).is_some_and(|b| !b.as_ptr().is_null()));
        // Not a directory, or missing.
        assert!(CfBundle::open(&macos.join("Real")).is_none());
        assert_eq!(executable(&dir.join("Missing.vst3")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
