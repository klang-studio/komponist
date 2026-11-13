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
//! The functions come straight from the C library (glibc ≥ 2.34 has `dlopen` in `libc.so.6`), which
//! keeps the crate free of a loader dependency.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::plugin::HostingError;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *mut c_char;
}

const RTLD_NOW: c_int = 2;
const RTLD_LOCAL: c_int = 0;
const RTLD_NODELETE: c_int = 0x1000;

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

    /// The raw handle (for VST3 `ModuleEntry`).
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
        // This test executable is no loadable library.
        let exe = std::env::current_exe().unwrap();
        assert!(unsafe { Library::open(&exe) }.is_err());
    }
}
