//! The plugin bridge: one process per hosted plugin instance (ARCHITECTURE §8.1–8.3).
//!
//! ```text
//! komponist --bridge --instance --control-fd <fd> [--rt-priority <1..=99>]
//! komponist --bridge --scan <bundle>
//! komponist-bridge --instance …   (the same, as a binary of its own)
//! ```
//!
//! Since WP-94 the bridge is a library: [`run`] is the whole process. The `komponist` binary calls
//! it first thing in `main()` when its first argument is `--bridge` (the app starts bridges from
//! its own executable, so a bridge can never be missing or stale next to it), and the small
//! `komponist-bridge` binary calls it with all its arguments. Either way the process is separate
//! from the app and initialises no GUI, graphics or audio.
//!
//! - **Instance mode** ([`instance`]): talks to the app over the inherited control socket, hosts
//!   one plugin on its main thread and processes blocks on an RT thread ([`rt`]) through the
//!   shared memory. A crashing plugin takes only this process down.
//! - **Scan mode** ([`scan`]): prints the bundle's plugins as JSON and exits.
//!
//! Both modes first turn core dumps off (`PR_SET_DUMPABLE = 0` on Linux, `RLIMIT_CORE = 0` on
//! macOS, where the bridge also clears its task-level crash exception ports): plugins crash on purpose in tests and by accident in practice, and systemd-coredump
//! would delay the crash detection to about 55 ms and pop up a crash reporter (PLAN WP-31).
//! `KOMPONIST_BRIDGE_COREDUMP=1` keeps core dumps for debugging a plugin crash. Any panic in the
//! bridge aborts it, so a failing bridge always looks like a crash to the app and never leaves a
//! half-torn-down plugin running. Both modes end with `_exit` after the plugin is torn down
//! ([`exit_now`]).

#[cfg(target_os = "macos")]
mod cf_loop;
#[cfg(target_os = "linux")]
mod event_loop;
mod instance;
mod notify;
mod rt;
mod scan;
mod sched;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!(
    "komponist-bridge: no main loop (event_loop.rs / cf_loop.rs) for this OS yet (Windows: W5, a Win32 message pump)"
);

use std::ffi::OsString;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::process::ExitCode;

// Debug builds: an allocation inside the RT loop (`assert_no_alloc`) aborts the bridge. Only the
// bridge's own Rust code is checked; plugin libraries bring their own allocator. The binaries
// (`komponist-bridge`, `komponist`) declare the allocator; this one is for the unit tests.
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// Whether bridges embed plugin GUIs on this OS (Linux: into the app's frame, macOS: into the
/// bridge's own frame window, #52). For the app and its tests, which do not link the hosting crate
/// directly.
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

const USAGE: &str = "usage: komponist-bridge --instance --control-fd <fd> [--rt-priority <1..=99>]
       komponist-bridge --scan <bundle>
       komponist --bridge <the same arguments>";

/// The main thread's name in bridge mode. On Linux it is the process name (`comm`, at most 15
/// bytes), by which tools and tests find the bridges of an app, whichever executable they were
/// started from. macOS names processes by their executable; there it names the main thread
/// (debuggers, `spindump`, crash reports).
const PROCESS_NAME: &std::ffi::CStr = c"komponist-bridge";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Mode {
    Instance(instance::Options),
    Scan(PathBuf),
    Help,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Mode, String> {
    let mut args = args.into_iter();
    let mut instance = false;
    let mut control_fd: Option<RawFd> = None;
    let mut rt_priority = sched::DEFAULT_PRIORITY;
    let mut scan: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.to_str() {
            Some("--instance") => instance = true,
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
            Some("--scan") => scan = Some(PathBuf::from(value("--scan")?)),
            Some("--help" | "-h") => return Ok(Mode::Help),
            _ => return Err(format!("unknown argument {}", arg.to_string_lossy())),
        }
    }
    match (instance, control_fd, scan) {
        (true, Some(control_fd), None) => Ok(Mode::Instance(instance::Options {
            control_fd,
            rt_priority,
        })),
        (true, _, Some(_)) => Err("--instance and --scan exclude each other".into()),
        (true, None, None) => Err("--instance needs --control-fd".into()),
        (false, Some(_), _) => Err("--control-fd needs --instance".into()),
        (false, None, Some(bundle)) => Ok(Mode::Scan(bundle)),
        (false, None, None) => Err("nothing to do".into()),
    }
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
        Ok(Mode::Instance(options)) => exit_now(instance::run(options)),
        Ok(Mode::Scan(bundle)) => exit_now(scan::run(&bundle)),
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
/// `PR_SET_DUMPABLE = 0` on Linux, `RLIMIT_CORE = 0` (the soft limit) on macOS.
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
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    compile_error!(
        "komponist-bridge: no process hardening (harden_process) for this OS yet (Windows: W5, SetErrorMode + WerSetFlags)"
    );
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
fn exit_now(code: u8) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: ends the process without running exit handlers; nothing in it is half-done.
    unsafe { libc::_exit(i32::from(code)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Mode, String> {
        parse_args(args.iter().map(OsString::from))
    }

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
