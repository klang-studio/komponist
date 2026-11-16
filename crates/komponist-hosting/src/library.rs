//! Plugin libraries: `dlopen` with the one policy both format layers follow.
//!
//! **A plugin library is never unmapped** (`RTLD_NODELETE`). Plugin code registers thread-exit
//! destructors (thread-locals, pthread keys) on the host threads that call it. Once its library
//! is unmapped, such a thread crashes when it exits: a test thread after its test, the bridge's
//! main thread in `exit()`. Seen in this crate's tests: VST3 fixtures unloaded with `dlclose`
//! crashed 12 of 20 parallel runs, the CLAP tests (clack-host `dlclose`s too) 3 of 60. Hosts call
//! `dlclose` (and VST3 `ModuleExit`) as usual; the code just stays mapped, and loading the library
//! again finds it in place.
//!
//! The functions come straight from the C library (glibc ≥ 2.34 has `dlopen` in `libc.so.6`, macOS
//! in `libSystem`) through the `libc` bindings, which keeps the crate free of a loader dependency.
//! The `RTLD_*` flags differ per OS (e.g. `RTLD_NODELETE` is `0x1000` on Linux and `0x80` on
//! macOS, `RTLD_LOCAL` is `0` and `0x4`), so they come from `libc` too, never as literals.

use std::ffi::{CStr, CString, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use libc::{RTLD_LOCAL, RTLD_NODELETE, RTLD_NOLOAD, RTLD_NOW, dlclose, dlerror, dlopen, dlsym};

use crate::plugin::HostingError;

/// The last `dlerror()` message.
fn last_error() -> String {
    // SAFETY: dlerror returns NULL or a NUL-terminated string valid until the next dl* call on
    // this thread.
    let message = unsafe { dlerror() };
    if message.is_null() {
        "unknown error".into()
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

fn c_path(path: &Path) -> Result<CString, HostingError> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| HostingError::Load(format!("{}: invalid path", path.display())))
}

/// A `dlopen` handle of a plugin library (`RTLD_NODELETE`); `dlclose`d on drop.
#[derive(Debug)]
pub(crate) struct Library {
    handle: *mut c_void,
}

// SAFETY: a dlopen handle is process-wide; `dlsym` and `dlclose` may be called from any thread.
unsafe impl Send for Library {}
// SAFETY: as above; `Library` has no interior mutability.
unsafe impl Sync for Library {}

impl Library {
    /// Loads `path` (its constructors run).
    ///
    /// # Safety
    /// Loading a plugin library runs foreign code; the bridge process exists to contain it.
    pub(crate) unsafe fn open(path: &Path) -> Result<Library, HostingError> {
        let c = c_path(path)?;
        // SAFETY: caller's contract.
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL | RTLD_NODELETE) };
        if handle.is_null() {
            return Err(HostingError::Load(format!(
                "{}: {}",
                path.display(),
                last_error()
            )));
        }
        Ok(Library { handle })
    }

    /// The raw handle (for VST3 `ModuleEntry` on Linux).
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn handle(&self) -> *mut c_void {
        self.handle
    }

    /// A function exported by the library, as `F`.
    ///
    /// # Safety
    /// `F` is a function pointer type matching the symbol's real signature.
    pub(crate) unsafe fn symbol<F: Copy>(&self, name: &CStr) -> Option<F> {
        debug_assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
        // SAFETY: the handle is live while `self` is.
        let ptr = unsafe { dlsym(self.handle, name.as_ptr()) };
        // SAFETY: caller's contract on `F`.
        (!ptr.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&ptr) })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle came from dlopen and is closed once.
        unsafe { dlclose(self.handle) };
    }
}

/// Marks an already loaded library (loaded by someone else, e.g. clack-host) as never to be
/// unmapped. A no-op if `path` is not loaded.
pub(crate) fn keep_mapped(path: &Path) {
    let Ok(c) = c_path(path) else {
        return;
    };
    // SAFETY: RTLD_NOLOAD loads nothing and runs no code; it only promotes the flags of a loaded
    // object. The extra reference is dropped again right away.
    let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_NOLOAD | RTLD_NODELETE) };
    if !handle.is_null() {
        // SAFETY: balances the dlopen above.
        unsafe { dlclose(handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_errors_and_noload() {
        let missing = Path::new("/nonexistent/lib.so");
        assert!(matches!(
            unsafe { Library::open(missing) },
            Err(HostingError::Load(m)) if m.contains("/nonexistent/lib.so")
        ));
        // Not loaded: nothing happens.
        keep_mapped(missing);
        // A file that is no library at all.
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/library.rs");
        assert!(unsafe { Library::open(&source) }.is_err());
        // This test executable is no loadable library for glibc (dyld on macOS hands out a handle
        // to the running main executable instead).
        #[cfg(target_os = "linux")]
        {
            let exe = std::env::current_exe().unwrap();
            assert!(unsafe { Library::open(&exe) }.is_err());
        }
    }

    /// The flags are the platform's own values (#44: the Linux literals were wrong on macOS).
    #[test]
    fn dlopen_flags_are_the_platform_values() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            (RTLD_NOW, RTLD_LOCAL, RTLD_NOLOAD, RTLD_NODELETE),
            (0x2, 0x0, 0x4, 0x1000)
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            (RTLD_NOW, RTLD_LOCAL, RTLD_NOLOAD, RTLD_NODELETE),
            (0x2, 0x4, 0x10, 0x80)
        );
    }

    /// A loaded system library opens with the policy flags, symbols resolve, and `keep_mapped`
    /// on it (`RTLD_NOLOAD | RTLD_NODELETE`) is accepted.
    #[test]
    fn a_system_library_opens_with_the_policy_flags() {
        #[cfg(target_os = "linux")]
        let path = Path::new("libc.so.6");
        #[cfg(target_os = "macos")]
        let path = Path::new("/usr/lib/libSystem.B.dylib");
        let lib = unsafe { Library::open(path) }.unwrap();
        type Getpid = unsafe extern "C" fn() -> i32;
        let getpid = unsafe { lib.symbol::<Getpid>(c"getpid") }.unwrap();
        assert_eq!(unsafe { getpid() } as u32, std::process::id());
        assert!(unsafe { lib.symbol::<Getpid>(c"no_such_symbol_here") }.is_none());
        keep_mapped(path);
        // RTLD_NOLOAD finds it loaded (on macOS a wrong flag value would load or fail instead).
        let c = c_path(path).unwrap();
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_NOLOAD) };
        assert!(!handle.is_null(), "{}", last_error());
        unsafe { dlclose(handle) };
    }
}
