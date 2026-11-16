//! Which ASIO drivers are installed, read from the registry (Windows).
//!
//! Every ASIO driver registers itself as a subkey of `HKEY_LOCAL_MACHINE\SOFTWARE\ASIO` (the subkey's
//! name is the driver's name, its `CLSID` value the COM class). That is the list the SDK's own
//! `asiolist.cpp` walks to load drivers, and this module reads it the same way: the default registry
//! view of the process, with `KEY_READ`. A 64-bit komponist therefore sees the 64-bit view, which is
//! the one that matters: it can only load 64-bit drivers, and those register there (32-bit drivers
//! register under `WOW6432Node` and are invisible to it, as they are to `asio-sys`).
//!
//! The check opens the key read-only and enumerates the names of the subkeys. It never loads a driver
//! (that is what slows cpal's device enumeration down and can disturb an interface another program
//! holds) and never writes to the registry (rule 14).

use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
};

/// The key ASIO drivers register under, below `HKEY_LOCAL_MACHINE`.
const ASIO_KEY: &str = r"SOFTWARE\ASIO";

/// Upper bound of the enumeration; a machine has a handful of drivers.
const MAX_SUBKEYS: u32 = 1024;

/// Whether at least one ASIO driver is registered for this process.
pub(crate) fn any_driver_registered() -> bool {
    subkey_count(HKEY_LOCAL_MACHINE, ASIO_KEY) > 0
}

/// The number of subkeys of `root\path`, counted up to [`MAX_SUBKEYS`]; 0 when the key does not exist
/// or cannot be read. Read-only: `KEY_READ` is the only access asked for.
fn subkey_count(root: HKEY, path: &str) -> u32 {
    let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: `path` is NUL-terminated and outlives the call; `key` receives a handle that is closed below.
    let opened = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ, &mut key) };
    if opened != ERROR_SUCCESS {
        return 0;
    }
    let mut count = 0;
    while count < MAX_SUBKEYS {
        // Only the existence of the subkey matters, so a name that does not fit is as good as one that does
        // (255 UTF-16 units is the registry's limit for a key name; the buffer has room for it).
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: `key` is open; `name` has `len` units; the optional class and time pointers are null.
        let status = unsafe {
            RegEnumKeyExW(
                key,
                count,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
            break; // ERROR_NO_MORE_ITEMS, or the key went away
        }
        count += 1;
    }
    // SAFETY: `key` was opened above and is not used afterwards.
    unsafe { RegCloseKey(key) };
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;

    /// An absent key means "no drivers" (and the test reads only: it cannot create the key).
    #[test]
    fn an_absent_key_has_no_subkeys() {
        assert_eq!(
            subkey_count(HKEY_CURRENT_USER, r"Software\komponist-test-nonexistent"),
            0
        );
        assert_eq!(
            subkey_count(HKEY_LOCAL_MACHINE, r"SOFTWARE\komponist-test-nonexistent"),
            0
        );
    }

    /// The enumeration finds subkeys of a key every Windows has (read-only).
    #[test]
    fn a_key_with_subkeys_is_counted() {
        assert!(subkey_count(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows") > 1);
    }

    /// The real check runs on any machine, with or without a driver, and agrees with the count.
    #[test]
    fn the_driver_check_follows_the_asio_key() {
        assert_eq!(
            any_driver_registered(),
            subkey_count(HKEY_LOCAL_MACHINE, ASIO_KEY) > 0
        );
    }
}
