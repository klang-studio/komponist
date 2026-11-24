//! The plugin bridge: one process per hosted plugin instance (ARCHITECTURE §8.1–8.3).
//!
//! ```text
//! komponist --bridge --instance --control-fd <fd> [--rt-priority <1..=99>]   (Linux, macOS)
//! komponist --bridge --instance [--rt-priority <1..=99>]                     (Windows)
//! komponist --bridge --scan <bundle>
//! komponist-bridge --instance …   (the same, as a binary of its own)
//! ```
//!
//! Since WP-94 the bridge is a library: [`run`] is the whole process. The `komponist` binary calls
//! it first thing in `main()` when its first argument is `--bridge` (the app starts bridges from
//! its own executable, so a bridge can never be missing or stale next to it), and the small
//! `komponist-bridge` binary calls it with all its arguments. Either way the process is separate
//! from the app and initialises no GUI, graphics or audio (on macOS: no Dock icon, no App Nap,
//! `macos_app`, #100).
//!
//! - **Instance mode** ([`instance`]): talks to the app over the inherited control socket, hosts
//!   one plugin on its main thread and processes blocks on an RT thread ([`rt`]) through the
//!   shared memory. A crashing plugin takes only this process down. On Windows (port W5) the
//!   control channel is the bridge's stdin (a named pipe the app passes as the child's stdin, so
//!   no handle value goes on the command line): there is no `--control-fd`, and the bridge points
//!   stdin at `NUL` once it holds the pipe (`win_stdio`). `--rt-priority` is accepted and ignored
//!   there (MMCSS has no priority number).
//! - **Scan mode** ([`scan`]): prints the bundle's plugins as JSON and exits.
//!
//! Both modes first turn core dumps off (`PR_SET_DUMPABLE = 0` on Linux, `RLIMIT_CORE = 0` on
//! macOS, where the bridge also clears its task-level crash exception ports): plugins crash on purpose in tests and by accident in practice, and systemd-coredump
//! would delay the crash detection to about 55 ms and pop up a crash reporter (PLAN WP-31).
//! On Windows the same goes for Windows Error Reporting: `SetErrorMode(SEM_FAILCRITICALERRORS |
//! SEM_NOGPFAULTERRORBOX)` and `WerSetFlags(WER_FAULT_REPORTING_NO_UI)`, so a crashing plugin never
//! leaves the bridge alive behind a dialog (both: the error mode covers unhandled exceptions, the
//! WER flag the paths that report without it, such as fast-fail from a plugin's `abort`).
//! `KOMPONIST_BRIDGE_COREDUMP=1` keeps core dumps (Windows: the WER report) for debugging a plugin
//! crash. Any panic in the
//! bridge aborts it, so a failing bridge always looks like a crash to the app and never leaves a
//! half-torn-down plugin running. Both modes end with `_exit` after the plugin is torn down
//! ([`exit_now`]).
//!
//! Both modes also end with the app, even while the main thread is stuck in a plugin call (#218):
//! on Linux and Windows the app sees to that when it spawns the bridge
//! (`komponist_ipc::process::spawn_tied`); on macOS the bridge watches the app (`app_watch`).

// Missing SAFETY comments: 2 (macOS build; Linux and Windows code not counted). Remove this allow when the crate is clean (#241 follow-up).
#![allow(clippy::undocumented_unsafe_blocks)]

#[cfg(target_os = "macos")]
mod app_watch;
#[cfg(target_os = "macos")]
mod cf_loop;
#[cfg(target_os = "linux")]
mod event_loop;
mod instance;
#[cfg(target_os = "macos")]
mod macos_app;
mod notify;
mod rt;
mod scan;
mod sched;
#[cfg(windows)]
mod win_loop;
#[cfg(windows)]
mod win_stdio;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!(
    "komponist-bridge: no main loop (event_loop.rs / cf_loop.rs / win_loop.rs) for this OS"
);

use std::ffi::OsString;
#[cfg(unix)]
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::process::ExitCode;

// Debug builds: an allocation inside the RT loop (`assert_no_alloc`) aborts the bridge. Only the
// bridge's own Rust code is checked; plugin libraries bring their own allocator. The binaries
// (`komponist-bridge`, `komponist`) declare the allocator; this one is for the unit tests.
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// Whether bridges embed plugin GUIs on this OS (Linux: into the app's frame; macOS and Windows:
/// into the bridge's own frame window, #52 and port W6). For the app and its tests, which do not
/// link the hosting crate directly.
pub use komponist_hosting::GUI_EMBEDDING;

/// Process exit codes.
pub mod exit {
    /// Done: shut down on request, or a scan printed its JSON.
    pub const SUCCESS: u8 = 0;
    /// The scan failed, or instance mode could not start (control socket, shared memory).
    pub const FAILURE: u8 = 1;
    /// Bad command line.
    pub const USAGE: u8 = 2;
    /// Scan: the bundle's format has no hosting (CLAP and VST3 both have it since WP-32; kept
    /// for formats to come).
    pub const UNSUPPORTED: u8 = 3;
}

/// Set to `1` to keep core dumps (the bridge is non-dumpable otherwise).
const COREDUMP_ENV: &str = "KOMPONIST_BRIDGE_COREDUMP";

#[cfg(unix)]
const USAGE: &str = "usage: komponist-bridge --instance --control-fd <fd> [--rt-priority <1..=99>]
       komponist-bridge --scan <bundle>
       komponist-bridge --scan-presets <clap-bundle>
       komponist-bridge --preset-state <clap-bundle> <selection>
       komponist --bridge <the same arguments>";
#[cfg(windows)]
const USAGE: &str =
    "usage: komponist-bridge --instance [--rt-priority <1..=99>]  (control pipe on stdin)
       komponist-bridge --scan <bundle>
       komponist-bridge --scan-presets <clap-bundle>
       komponist-bridge --preset-state <clap-bundle> <selection>
       komponist --bridge <the same arguments>";

/// The main thread's name in bridge mode. On Linux it is the process name (`comm`, at most 15
/// bytes), by which tools and tests find the bridges of an app, whichever executable they were
/// started from. macOS names processes by their executable; there it names the main thread
/// (debuggers, `spindump`, crash reports), as on Windows (`SetThreadDescription`).
const PROCESS_NAME: &std::ffi::CStr = c"komponist-bridge";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Mode {
    Instance(instance::Options),
    Scan(PathBuf),
    ScanPresets(PathBuf),
    PresetState(PathBuf, String),
    Help,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Mode, String> {
    let mut args = args.into_iter();
    let mut instance = false;
    #[cfg(unix)]
    let mut control_fd: Option<RawFd> = None;
    let mut rt_priority = sched::DEFAULT_PRIORITY;
    let mut scan = None;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.to_str() {
            Some("--instance") => instance = true,
            #[cfg(unix)]
            Some("--control-fd") => {
                let v = value("--control-fd")?;
                let fd = v
                    .to_str()
                    .and_then(|s| s.parse::<RawFd>().ok())
                    .filter(|fd| *fd >= 0)
                    .ok_or_else(|| format!("--control-fd: not an fd: {}", v.to_string_lossy()))?;
                control_fd = Some(fd);
            }
            Some("--rt-priority") => {
                let v = value("--rt-priority")?;
                rt_priority = v
                    .to_str()
                    .and_then(|s| s.parse::<i32>().ok())
                    .filter(|p| sched::PRIORITY_RANGE.contains(p))
                    .ok_or_else(|| {
                        format!("--rt-priority: not in 1..=99: {}", v.to_string_lossy())
                    })?;
            }
            Some("--scan") => scan = Some(Mode::Scan(PathBuf::from(value("--scan")?))),
            Some("--scan-presets") => {
                scan = Some(Mode::ScanPresets(PathBuf::from(value("--scan-presets")?)));
            }
            Some("--preset-state") => {
                let bundle = PathBuf::from(value("--preset-state")?);
                let selection = value("--preset-state")?
                    .into_string()
                    .map_err(|_| "--preset-state needs a UTF-8 selection")?;
                scan = Some(Mode::PresetState(bundle, selection));
            }
            Some("--help" | "-h") => return Ok(Mode::Help),
            _ => return Err(format!("unknown argument {}", arg.to_string_lossy())),
        }
    }
    #[cfg(unix)]
    return match (instance, control_fd, scan) {
        (true, Some(control_fd), None) => Ok(Mode::Instance(instance::Options {
            control_fd,
            rt_priority,
        })),
        (true, _, Some(_)) => Err("--instance and the scan modes exclude each other".into()),
        (true, None, None) => Err("--instance needs --control-fd".into()),
        (false, Some(_), _) => Err("--control-fd needs --instance".into()),
        (false, None, Some(scan)) => Ok(scan),
        (false, None, None) => Err("nothing to do".into()),
    };
    #[cfg(windows)]
    return match (instance, scan) {
        (true, None) => Ok(Mode::Instance(instance::Options { rt_priority })),
        (true, Some(_)) => Err("--instance and the scan modes exclude each other".into()),
        (false, Some(scan)) => Ok(scan),
        (false, None) => Err("nothing to do".into()),
    };
}

/// The bridge process: `args` are the command line without the program name (and without the
/// `--bridge` of `komponist`). Returns the exit code of the process for `--help` and bad
/// arguments; instance and scan mode end the process themselves ([`exit_now`]).
pub fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    harden_process();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::abort();
    }));
    match parse_args(args) {
        Ok(Mode::Instance(options)) => {
            #[cfg(target_os = "macos")]
            {
                app_watch::start();
                macos_app::setup();
            }
            exit_now(instance::run(options))
        }
        Ok(Mode::Scan(bundle)) => {
            #[cfg(target_os = "macos")]
            app_watch::start();
            exit_now(scan::run(&bundle))
        }
        Ok(Mode::ScanPresets(bundle)) => {
            #[cfg(target_os = "macos")]
            app_watch::start();
            exit_now(scan::run_presets(&bundle))
        }
        Ok(Mode::PresetState(bundle, selection)) => {
            #[cfg(target_os = "macos")]
            app_watch::start();
            exit_now(scan::run_preset_state(&bundle, &selection))
        }
        Ok(Mode::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("komponist-bridge: {message}\n{USAGE}");
            ExitCode::from(exit::USAGE)
        }
    }
}

/// Names the process (or main thread) and keeps core dumps off, unless [`COREDUMP_ENV`] is `1`:
/// `PR_SET_DUMPABLE = 0` on Linux, `RLIMIT_CORE = 0` (the soft limit) on macOS, no Windows Error
/// Reporting UI on Windows.
fn harden_process() {
    let keep_cores = std::env::var_os(COREDUMP_ENV).is_some_and(|v| v == "1");
    #[cfg(target_os = "linux")]
    {
        // `komponist --bridge` shows up as `komponist` otherwise.
        let _ = rustix::thread::set_name(PROCESS_NAME);
        if !keep_cores {
            let _ = rustix::process::set_dumpable_behavior(
                rustix::process::DumpableBehavior::NotDumpable,
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: names the calling thread; the string is NUL-terminated and static.
        unsafe { libc::pthread_setname_np(PROCESS_NAME.as_ptr()) };
        if !keep_cores {
            detach_from_crash_reporter();
            use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
            let hard = getrlimit(Resource::Core).maximum;
            let _ = setrlimit(
                Resource::Core,
                Rlimit {
                    current: Some(0),
                    maximum: hard,
                },
            );
        }
    }
    #[cfg(windows)]
    {
        name_current_thread(PROCESS_NAME);
        if !keep_cores {
            disable_error_reporting();
        }
    }
}

/// Windows: names the calling thread (`SetThreadDescription`; debuggers, crash dumps, Process
/// Explorer). A failure only leaves it unnamed.
#[cfg(windows)]
fn name_current_thread(name: &std::ffi::CStr) {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadDescription};
    let wide: Vec<u16> = name
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the pseudo handle of this thread and a NUL-terminated string.
    unsafe { SetThreadDescription(GetCurrentThread(), wide.as_ptr()) };
}

/// Windows: no crash dialog, no Windows Error Reporting UI (module docs). The error mode is
/// inherited by processes the plugin starts.
#[cfg(windows)]
fn disable_error_reporting() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };
    use windows_sys::Win32::System::ErrorReporting::{WER_FAULT_REPORTING_NO_UI, WerSetFlags};
    // SAFETY: plain process-wide settings.
    unsafe {
        SetErrorMode(GetErrorMode() | SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        WerSetFlags(WER_FAULT_REPORTING_NO_UI);
    }
}

/// Detaches the bridge from ReportCrash as far as a task can (#52): clears the task-level crash
/// exception ports a launcher may have handed down, so a crash is not delivered to a reporter
/// through them; the app reports bridge crashes itself ("Abgestürzt"). A handler registered at
/// host level (how ReportCrash gets crashes of processes started from a terminal: their task ports
/// are already empty, measured 2026-11-16) is out of a task's reach; whether the macOS dialog
/// still appears for a crashing bridge with a frame window is checked on screen.
#[cfg(target_os = "macos")]
fn detach_from_crash_reporter() {
    unsafe extern "C" {
        fn task_set_exception_ports(
            task: libc::mach_port_t,
            exception_mask: u32,
            new_port: libc::mach_port_t,
            behavior: i32,
            new_flavor: i32,
        ) -> i32;
        /// The task's own port (what `mach_task_self()` returns).
        static mach_task_self_: libc::mach_port_t;
    }
    /// `EXC_MASK_CRASH | EXC_MASK_CORPSE_NOTIFY`.
    const CRASH_MASKS: u32 = (1 << 10) | (1 << 13);
    /// `EXCEPTION_DEFAULT`.
    const EXCEPTION_DEFAULT: i32 = 1;
    /// `THREAD_STATE_NONE` on arm64.
    const THREAD_STATE_NONE: i32 = 5;
    // SAFETY: changes only this task's exception ports; a null port removes the handler.
    unsafe {
        task_set_exception_ports(
            mach_task_self_,
            CRASH_MASKS,
            0,
            EXCEPTION_DEFAULT,
            THREAD_STATE_NONE,
        );
    }
}

/// Ends the process with `code` after instance or scan mode tore the plugin down: `_exit`, not
/// `exit`. Plugins register thread-local destructors and `atexit` handlers on the main thread, and
/// some fail when they run after the plugin is gone (Klopfer panics in its TLS destructors on
/// `process::exit`, spike #42); the bridge has nothing left to clean up by then. Rust's buffered
/// stdout is flushed first (the scan writes its JSON unbuffered anyway).
///
/// Windows: `TerminateProcess(GetCurrentProcess(), code)`. The CRT's `_exit` skips `atexit`
/// handlers but ends in `ExitProcess`, which still sends `DLL_PROCESS_DETACH` to every DLL and runs
/// the TLS callbacks (where Rust's thread-local destructors live), i.e. plugin code after the
/// plugin is gone; `TerminateProcess` runs none of it.
fn exit_now(code: u8) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    // SAFETY: ends the process without running exit handlers; nothing in it is half-done.
    unsafe {
        libc::_exit(i32::from(code))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        // SAFETY: ends this process with `code`; nothing in it is half-done.
        unsafe { TerminateProcess(GetCurrentProcess(), u32::from(code)) };
        // TerminateProcess on the own process does not return; should it ever, fail hard.
        std::process::abort()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Mode, String> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn preset_scan_uses_the_disposable_scan_mode() {
        assert_eq!(
            parse(&["--scan-presets", "Community.clap"]),
            Ok(Mode::ScanPresets("Community.clap".into()))
        );
        assert!(parse(&["--scan-presets"]).is_err());
        assert!(parse(&["--instance", "--scan-presets", "Community.clap"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn instance_and_scan_arguments() {
        assert_eq!(
            parse(&["--instance", "--control-fd", "7"]),
            Ok(Mode::Instance(instance::Options {
                control_fd: 7,
                rt_priority: sched::DEFAULT_PRIORITY,
            }))
        );
        assert_eq!(
            parse(&["--control-fd", "3", "--rt-priority", "70", "--instance"]),
            Ok(Mode::Instance(instance::Options {
                control_fd: 3,
                rt_priority: 70,
            }))
        );
        assert_eq!(
            parse(&["--scan", "/usr/lib/clap/Dexed.clap"]),
            Ok(Mode::Scan("/usr/lib/clap/Dexed.clap".into()))
        );
        assert_eq!(parse(&["--help"]), Ok(Mode::Help));
    }

    /// Windows: the control pipe is stdin, so instance mode takes no fd argument.
    #[cfg(windows)]
    #[test]
    fn instance_and_scan_arguments() {
        assert_eq!(
            parse(&["--instance"]),
            Ok(Mode::Instance(instance::Options {
                rt_priority: sched::DEFAULT_PRIORITY,
            }))
        );
        assert_eq!(
            parse(&["--rt-priority", "70", "--instance"]),
            Ok(Mode::Instance(instance::Options { rt_priority: 70 }))
        );
        assert_eq!(
            parse(&["--scan", r"C:\Program Files\Common Files\CLAP\Dexed.clap"]),
            Ok(Mode::Scan(
                r"C:\Program Files\Common Files\CLAP\Dexed.clap".into()
            ))
        );
        assert_eq!(parse(&["--help"]), Ok(Mode::Help));
        for args in [
            &[][..],
            &["--instance", "--control-fd", "3"],
            &["--instance", "--rt-priority", "0"],
            &["--instance", "--scan", r"C:\x.clap"],
            &["--scan"],
            &["--bogus"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    /// macOS: the main thread is named and core dumps are off (the soft limit only).
    #[cfg(target_os = "macos")]
    #[test]
    fn hardening_names_the_thread_and_turns_core_dumps_off() {
        if std::env::var_os(COREDUMP_ENV).is_some() {
            return;
        }
        // On a thread of its own: the name is per thread.
        std::thread::spawn(|| {
            use rustix::process::{Resource, getrlimit};
            let hard = getrlimit(Resource::Core).maximum;
            harden_process();
            let limit = getrlimit(Resource::Core);
            assert_eq!((limit.current, limit.maximum), (Some(0), hard));
            let mut name = [0 as std::ffi::c_char; 64];
            // SAFETY: the buffer's length is passed.
            let rc = unsafe {
                libc::pthread_getname_np(libc::pthread_self(), name.as_mut_ptr(), name.len())
            };
            assert_eq!(rc, 0);
            // SAFETY: NUL-terminated by pthread_getname_np.
            let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
            assert_eq!(name, PROCESS_NAME);
        })
        .join()
        .unwrap();
    }

    /// Windows: the thread is named and the crash dialogs are off. (Process-wide error mode: it
    /// only suppresses dialogs, which no test wants anyway.)
    #[cfg(windows)]
    #[test]
    fn hardening_names_the_thread_and_turns_crash_dialogs_off() {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadDescription};
        if std::env::var_os(COREDUMP_ENV).is_some() {
            return;
        }
        std::thread::spawn(|| {
            harden_process();
            let mode = unsafe { GetErrorMode() };
            assert_eq!(
                mode & (SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX),
                SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX
            );
            let mut name: *mut u16 = std::ptr::null_mut();
            // SAFETY: the pseudo handle of this thread; the string is freed with LocalFree.
            assert!(unsafe { GetThreadDescription(GetCurrentThread(), &mut name) } >= 0);
            let len = (0..).take_while(|&i| unsafe { *name.add(i) } != 0).count();
            let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(name, len) });
            unsafe { windows_sys::Win32::Foundation::LocalFree(name.cast()) };
            assert_eq!(text, "komponist-bridge");
        })
        .join()
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bad_arguments_are_refused() {
        for args in [
            &[][..],
            &["--instance"],
            &["--instance", "--control-fd"],
            &["--instance", "--control-fd", "x"],
            &["--instance", "--control-fd", "-1"],
            &["--instance", "--control-fd", "3", "--rt-priority", "0"],
            &["--instance", "--control-fd", "3", "--rt-priority", "100"],
            &["--instance", "--control-fd", "3", "--scan", "/x.clap"],
            &["--scan"],
            &["--control-fd", "3"],
            &["--bogus"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
}
