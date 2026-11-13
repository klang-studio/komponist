//! `komponist-bridge`: one process per hosted plugin instance (ARCHITECTURE §8.1–8.3).
//!
//! ```text
//! komponist-bridge --instance --control-fd <fd> [--rt-priority <1..=99>]
//! komponist-bridge --scan <bundle>
//! ```
//!
//! - **Instance mode** ([`instance`]): talks to the app over the inherited control socket, hosts
//!   one plugin on its main thread and processes blocks on an RT thread ([`rt`]) through the
//!   shared memory. A crashing plugin takes only this process down.
//! - **Scan mode** ([`scan`]): prints the bundle's plugins as JSON and exits.
//!
//! Both modes first make the process non-dumpable (`PR_SET_DUMPABLE = 0`): plugins crash on
//! purpose in tests and by accident in practice, and systemd-coredump would delay the crash
//! detection to about 55 ms and pop up a crash reporter (PLAN WP-31). `KOMPONIST_BRIDGE_COREDUMP=1`
//! keeps core dumps for debugging a plugin crash. Any panic in the bridge aborts it, so a failing
//! bridge always looks like a crash to the app and never leaves a half-torn-down plugin running.

mod event_loop;
mod instance;
mod notify;
mod rt;
mod scan;
mod sched;

use std::ffi::OsString;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::process::ExitCode;

// Debug builds: an allocation inside the RT loop (`assert_no_alloc`) aborts the bridge. Only the
// bridge's own Rust code is checked; plugin libraries bring their own allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// Process exit codes.
pub mod exit {
    /// The scan failed, or instance mode could not start (control socket, shared memory).
    pub const FAILURE: u8 = 1;
    /// Bad command line.
    pub const USAGE: u8 = 2;
    /// Scan: the bundle's format has no hosting yet (VST3 until WP-32).
    pub const UNSUPPORTED: u8 = 3;
}

/// Set to `1` to keep core dumps (the bridge is non-dumpable otherwise).
const COREDUMP_ENV: &str = "KOMPONIST_BRIDGE_COREDUMP";

const USAGE: &str = "usage: komponist-bridge --instance --control-fd <fd> [--rt-priority <1..=99>]
       komponist-bridge --scan <bundle>";

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

fn main() -> ExitCode {
    if std::env::var_os(COREDUMP_ENV).is_none_or(|v| v != "1") {
        let _ =
            rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::abort();
    }));
    match parse_args(std::env::args_os().skip(1)) {
        Ok(Mode::Instance(options)) => instance::run(options),
        Ok(Mode::Scan(bundle)) => scan::run(&bundle),
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
