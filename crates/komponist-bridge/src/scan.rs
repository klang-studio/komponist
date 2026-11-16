//! Scan mode: `komponist-bridge --scan <bundle>` (ARCHITECTURE §8.3).
//!
//! Prints the bundle's plugins as one JSON array of `PluginDescriptor` (serde_json, the field names
//! of komponist-ipc) and a newline on stdout. No GUI, no audio thread, no control socket. Plugins
//! that print while loading cannot corrupt the JSON: stdout is moved aside first and fd 1 points
//! at stderr while plugin code runs.
//!
//! Exit codes ([`crate::exit`]): 0 with the JSON (possibly `[]`), [`crate::exit::FAILURE`] when
//! the bundle cannot be scanned, [`crate::exit::UNSUPPORTED`] for a format without hosting (none
//! since WP-32 added VST3), [`crate::exit::USAGE`] for bad arguments. The reason goes to stderr. A
//! crashing bundle kills the scan process with its signal.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use komponist_hosting::HostingError;

use crate::exit;

/// Runs a scan of `bundle`; the process exit code ([`crate::exit`]).
pub fn run(bundle: &Path) -> u8 {
    // The JSON travels as UTF-8 and the app keys its cache by the path (WP-45 note).
    if bundle.to_str().is_none() {
        eprintln!(
            "komponist-bridge: {}: path is not valid UTF-8",
            bundle.display()
        );
        return exit::FAILURE;
    }
    let mut out = match json_output() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("komponist-bridge: stdout: {e}");
            return exit::FAILURE;
        }
    };
    match komponist_hosting::scan(bundle) {
        Ok(descriptors) => {
            let written = serde_json::to_writer(&mut out, &descriptors)
                .map_err(std::io::Error::from)
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush());
            match written {
                Ok(()) => exit::SUCCESS,
                Err(e) => {
                    eprintln!("komponist-bridge: writing the scan result: {e}");
                    exit::FAILURE
                }
            }
        }
        Err(HostingError::Unsupported(reason)) => {
            eprintln!(
                "komponist-bridge: {}: unsupported: {reason}",
                bundle.display()
            );
            exit::UNSUPPORTED
        }
        Err(e) => {
            eprintln!("komponist-bridge: {}: {e}", bundle.display());
            exit::FAILURE
        }
    }
}

/// A duplicate of stdout for the JSON; fd 1 itself then points at stderr.
fn json_output() -> std::io::Result<File> {
    let out = rustix::io::dup(rustix::stdio::stdout())?;
    rustix::stdio::dup2_stdout(rustix::stdio::stderr())?;
    Ok(File::from(out))
}
