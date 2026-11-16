//! Plugin libraries: `dlopen` (Windows: `LoadLibraryExW`) with the one policy both format layers
//! follow.
//!
//! **A plugin library is never unmapped** (`RTLD_NODELETE`). Plugin code registers thread-exit
//! destructors (thread-locals, pthread keys) on the host threads that call it. Once its library
//! is unmapped, such a thread crashes when it exits: a test thread after its test, the bridge's
//! main thread in `exit()`. Seen in this crate's tests: VST3 fixtures unloaded with `dlclose`
//! crashed 12 of 20 parallel runs, the CLAP tests (clack-host `dlclose`s too) 3 of 60. Hosts call
//! `dlclose` (and VST3 `ModuleExit`) as usual; the code just stays mapped, and loading the library
//! again finds it in place.
//!
//! Linux and macOS: the functions come straight from the C library (glibc ≥ 2.34 has `dlopen` in
//! `libc.so.6`, macOS in `libSystem`) through the `libc` bindings, which keeps the crate free of a
//! loader dependency. The `RTLD_*` flags differ per OS (e.g. `RTLD_NODELETE` is `0x1000` on Linux
//! and `0x80` on macOS, `RTLD_LOCAL` is `0` and `0x4`), so they come from `libc` too, never as
//! literals.
//!
//! Windows (port W5): `LoadLibraryExW(path, NULL, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR |
//! LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)`: the plugin's own folder and the system folders are searched
//! for its dependencies, never the current directory or `PATH` (a DLL of the same name elsewhere
//! cannot slip in). The flag needs an absolute path, so relative ones are made absolute first.
//! `GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN)` is the `RTLD_NODELETE` of Windows: a
//! pinned module stays loaded until the process ends, whatever `FreeLibrary` calls follow.
//! `GetProcAddress` replaces `dlsym`.

use std::path::Path;

use crate::plugin::HostingError;

pub(crate) use sys::{Library, keep_mapped};

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("komponist-hosting: no plugin-library loader (library.rs) for this OS");

/// `HostingError::Load` for `path` with `reason`.
fn load_error(path: &Path, reason: impl core::fmt::Display) -> HostingError {
    HostingError::Load(format!("{}: {reason}", path.display()))
}

#[cfg(unix)]
mod sys {
    use std::ffi::{CStr, CString, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use libc::{RTLD_LOCAL, RTLD_NODELETE, RTLD_NOLOAD, RTLD_NOW, dlclose, dlerror, dlopen, dlsym};

    use super::load_error;
    use crate::plugin::HostingError;

    /// The last `dlerror()` message.
    pub(super) fn last_error() -> String {
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

    pub(super) fn c_path(path: &Path) -> Result<CString, HostingError> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| load_error(path, "invalid path"))
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
                return Err(load_error(path, last_error()));
            }
            Ok(Library { handle })
        }

        /// The raw handle (for VST3 `ModuleEntry` on Linux).
        #[cfg(target_os = "linux")]
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
        // SAFETY: RTLD_NOLOAD loads nothing and runs no code; it only promotes the flags of a
        // loaded object. The extra reference is dropped again right away.
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_NOLOAD | RTLD_NODELETE) };
        if !handle.is_null() {
            // SAFETY: balances the dlopen above.
            unsafe { dlclose(handle) };
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::{CStr, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW,
        GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        LoadLibraryExW,
    };

    use super::load_error;
    use crate::plugin::HostingError;

    /// `path` as an absolute, NUL-terminated UTF-16 string (`LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR`
    /// refuses relative paths).
    pub(super) fn wide_path(path: &Path) -> Result<Vec<u16>, HostingError> {
        let absolute = std::path::absolute(path).map_err(|e| load_error(path, e))?;
        let wide: Vec<u16> = absolute.as_os_str().encode_wide().chain([0]).collect();
        if wide[..wide.len() - 1].contains(&0) {
            return Err(load_error(path, "invalid path"));
        }
        Ok(wide)
    }

    /// Pins the module at `module` (its base address): it stays loaded until the process ends.
    fn pin(module: HMODULE) -> bool {
        let mut pinned: HMODULE = core::ptr::null_mut();
        // SAFETY: FROM_ADDRESS looks the module up by an address inside it (its base); nothing is
        // loaded, and the pinned module needs no matching FreeLibrary.
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                module as *const u16,
                &mut pinned,
            ) != 0
        }
    }

    /// A loaded and pinned plugin DLL; `FreeLibrary`d on drop (which leaves it loaded).
    #[derive(Debug)]
    pub(crate) struct Library {
        module: HMODULE,
    }

    // SAFETY: a module handle is process-wide; `GetProcAddress` and `FreeLibrary` may be called
    // from any thread.
    unsafe impl Send for Library {}
    // SAFETY: as above; `Library` has no interior mutability.
    unsafe impl Sync for Library {}

    impl Library {
        /// Loads `path` (its `DllMain` runs) and pins it.
        ///
        /// # Safety
        /// Loading a plugin library runs foreign code; the bridge process exists to contain it.
        pub(crate) unsafe fn open(path: &Path) -> Result<Library, HostingError> {
            let wide = wide_path(path)?;
            // SAFETY: caller's contract; a NUL-terminated absolute path.
            let module = unsafe {
                LoadLibraryExW(
                    wide.as_ptr(),
                    core::ptr::null_mut(),
                    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                )
            };
            if module.is_null() {
                return Err(load_error(path, std::io::Error::last_os_error()));
            }
            if !pin(module) {
                let error = std::io::Error::last_os_error();
                // SAFETY: balances the LoadLibraryExW above.
                unsafe { FreeLibrary(module) };
                return Err(load_error(path, format!("pinning the module: {error}")));
            }
            Ok(Library { module })
        }

        /// A function exported by the library, as `F`.
        ///
        /// # Safety
        /// `F` is a function pointer type matching the symbol's real signature.
        pub(crate) unsafe fn symbol<F: Copy>(&self, name: &CStr) -> Option<F> {
            debug_assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
            // SAFETY: the module is loaded while `self` lives; `name` is NUL-terminated.
            let ptr = unsafe { GetProcAddress(self.module, name.as_ptr().cast()) }?;
            // SAFETY: caller's contract on `F`.
            Some(unsafe { std::mem::transmute_copy(&ptr) })
        }
    }

    impl Drop for Library {
        fn drop(&mut self) {
            // SAFETY: balances the LoadLibraryExW of `open`; the pin keeps the code loaded.
            unsafe { FreeLibrary(self.module) };
        }
    }

    /// Pins an already loaded DLL (loaded by someone else, e.g. clack-host) so it is never
    /// unloaded. A no-op if `path` is not loaded.
    pub(crate) fn keep_mapped(path: &Path) {
        let Ok(wide) = wide_path(path) else {
            return;
        };
        let mut module: HMODULE = core::ptr::null_mut();
        // SAFETY: looks a loaded module up by its path; loads nothing and runs no code.
        unsafe {
            GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN, wide.as_ptr(), &mut module);
        }
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
            Err(HostingError::Load(m)) if m.contains("lib.so")
        ));
        // Not loaded: nothing happens.
        keep_mapped(missing);
        // A file that is no library at all.
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/library.rs");
        assert!(unsafe { Library::open(&source) }.is_err());
        // This test executable is no loadable library for glibc (dyld on macOS hands out a handle
        // to the running main executable instead, and Windows the running EXE's module).
        #[cfg(target_os = "linux")]
        {
            let exe = std::env::current_exe().unwrap();
            assert!(unsafe { Library::open(&exe) }.is_err());
        }
    }

    /// The flags are the platform's own values (#44: the Linux literals were wrong on macOS).
    #[cfg(unix)]
    #[test]
    fn dlopen_flags_are_the_platform_values() {
        use libc::{RTLD_LOCAL, RTLD_NODELETE, RTLD_NOLOAD, RTLD_NOW};
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
    #[cfg(unix)]
    #[test]
    fn a_system_library_opens_with_the_policy_flags() {
        use libc::{RTLD_NOLOAD, RTLD_NOW, dlclose, dlopen};
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
        let c = sys::c_path(path).unwrap();
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_NOLOAD) };
        assert!(!handle.is_null(), "{}", sys::last_error());
        unsafe { dlclose(handle) };
    }

    /// Windows: a system DLL opens by its absolute path, symbols resolve, and the module stays
    /// loaded after the `Library` is dropped (the pin).
    #[cfg(windows)]
    #[test]
    fn a_system_library_opens_and_stays_loaded() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        // A DLL a test process does not load on its own, so the pin is what keeps it.
        let path = Path::new(&root).join(r"System32\winmm.dll");
        let lib = unsafe { Library::open(&path) }.unwrap();
        type TimeGetTime = unsafe extern "system" fn() -> u32;
        let time = unsafe { lib.symbol::<TimeGetTime>(c"timeGetTime") };
        assert!(time.is_some_and(|f| unsafe { f() } > 0));
        assert!(unsafe { lib.symbol::<TimeGetTime>(c"no_such_symbol_here") }.is_none());
        drop(lib);
        let wide = sys::wide_path(&path).unwrap();
        assert!(!unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null());
        // `keep_mapped` on a loaded and on an unloaded path does nothing harmful.
        keep_mapped(&path);
        keep_mapped(Path::new(r"C:\nonexistent\x.dll"));
        // A relative path is made absolute (LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR needs that).
        assert!(matches!(
            unsafe { Library::open(Path::new("no-such-plugin.dll")) },
            Err(HostingError::Load(m)) if m.contains("no-such-plugin.dll")
        ));
    }
}
