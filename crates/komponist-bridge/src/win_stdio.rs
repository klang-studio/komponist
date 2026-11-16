//! The bridge's standard handles on Windows (port W5).
//!
//! A Windows process has its standard handles twice: the process's (`GetStdHandle`, used by
//! Rust's `std::io` and by plugins that write through Win32) and the C runtime's file descriptors
//! 0–2 (`printf`, `std::cout`), which the UCRT copies from the former at start-up. A plugin linked
//! against the shared UCRT uses the process's descriptor table; one with a static CRT initialises
//! its own table from `GetStdHandle` when its DLL loads. Both are changed here, before any plugin
//! code runs, so every way of printing ends up where it should:
//!
//! - [`take_control_pipe`] (instance mode): the control pipe is the bridge's stdin. The bridge
//!   keeps a non-inheritable duplicate, then points both stdins at `NUL` and closes the original
//!   handle, so no plugin can read the control pipe and no helper process a plugin starts inherits
//!   it (a dead bridge must close the pipe for the app).
//! - [`move_stdout_to_stderr`] (scan mode): the JSON goes to a duplicate of stdout; both stdouts
//!   then point at stderr (Dexed prints a banner on load).

use std::fs::File;
use std::io;
use std::os::windows::io::{AsHandle, IntoRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};

/// A standard handle as a raw value, `None` if there is none.
fn std_handle(which: STD_HANDLE) -> Option<RawHandle> {
    // SAFETY: plain FFI call.
    let handle = unsafe { GetStdHandle(which) };
    (!handle.is_null() && handle as isize != -1).then_some(handle)
}

/// Whether the CRT's descriptor `fd` holds `handle`.
fn crt_holds(fd: i32, handle: RawHandle) -> bool {
    // SAFETY: only reads the CRT's descriptor table (-1 or -2 for a closed descriptor).
    unsafe { libc::get_osfhandle(fd) == handle as isize }
}

/// Points the CRT's descriptor `fd` at a fresh handle of `NUL`. `_dup2` closes what `fd` held.
fn crt_to_nul(fd: i32, flags: i32) -> io::Result<()> {
    let nul = File::options()
        .read(flags == libc::O_RDONLY)
        .write(flags != libc::O_RDONLY)
        .open("NUL")?;
    // SAFETY: the CRT takes the handle over (closed again by `close` below).
    let temp =
        unsafe { libc::open_osfhandle(nul.into_raw_handle() as isize, flags | libc::O_BINARY) };
    if temp < 0 {
        return Err(io::Error::other("_open_osfhandle failed"));
    }
    // SAFETY: valid descriptors; `dup2` makes `fd` a duplicate of `temp`.
    let duplicated = unsafe { libc::dup2(temp, fd) } == 0;
    // SAFETY: the temporary descriptor is ours.
    unsafe { libc::close(temp) };
    if duplicated {
        Ok(())
    } else {
        Err(io::Error::other("_dup2 failed"))
    }
}

/// Instance mode: takes the control pipe from stdin (see the module docs).
pub fn take_control_pipe() -> io::Result<OwnedHandle> {
    let original = std_handle(STD_INPUT_HANDLE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no stdin handle"))?;
    // A file or `NUL` would read as an endless stream of empty reads.
    // SAFETY: plain query on a valid handle.
    if unsafe { GetFileType(original) } != FILE_TYPE_PIPE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdin is no pipe",
        ));
    }
    // Not inheritable (DuplicateHandle without inheritance).
    let pipe = io::stdin().as_handle().try_clone_to_owned()?;
    let crt_owns_original = crt_holds(0, original);
    // The CRT's stdin: `_dup2` also closes the original handle when the CRT held it.
    let crt = crt_to_nul(0, libc::O_RDONLY);
    let nul = File::open("NUL")?;
    // SAFETY: a valid handle that stays open for the rest of the process (leaked on purpose).
    unsafe { SetStdHandle(STD_INPUT_HANDLE, nul.into_raw_handle()) };
    if !(crt_owns_original && crt.is_ok()) {
        // SAFETY: the inherited original, owned by nobody else now.
        unsafe { CloseHandle(original) };
    }
    Ok(pipe)
}

/// Scan mode: a duplicate of stdout for the JSON; both stdouts then point at stderr (or `NUL`
/// when there is no stderr).
pub fn move_stdout_to_stderr() -> io::Result<File> {
    let original = std_handle(STD_OUTPUT_HANDLE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no stdout handle"))?;
    let out = io::stdout().as_handle().try_clone_to_owned()?;
    if std_handle(STD_ERROR_HANDLE) == Some(original) {
        // One handle for both (`2>&1`): plugin output and the JSON share it whatever happens, and
        // redirecting would close stderr.
        return Ok(File::from(out));
    }
    let crt_owns_original = crt_holds(1, original);
    // SAFETY: valid descriptor numbers; `dup2` closes what fd 1 held (the original stdout).
    let crt = if unsafe { libc::dup2(2, 1) } == 0 {
        Ok(())
    } else {
        crt_to_nul(1, libc::O_WRONLY)
    };
    let target = match std_handle(STD_ERROR_HANDLE) {
        Some(stderr) => stderr,
        None => File::options().write(true).open("NUL")?.into_raw_handle(),
    };
    // SAFETY: a valid handle (stderr's, or a NUL handle leaked on purpose).
    unsafe { SetStdHandle(STD_OUTPUT_HANDLE, target) };
    if !(crt_owns_original && crt.is_ok()) {
        // SAFETY: the inherited original, owned by nobody else now.
        unsafe { CloseHandle(original) };
    }
    Ok(File::from(out))
}
