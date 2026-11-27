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
    run_output(bundle, true, |out| {
        let descriptors = komponist_hosting::scan(bundle)?;
        serde_json::to_writer(out, &descriptors)
            .map_err(|error| HostingError::State(error.to_string()))
    })
}

/// Runs CLAP preset discovery in the same isolation as the plug-in descriptor scan.
pub fn run_presets(bundle: &Path) -> u8 {
    run_output(bundle, true, |out| {
        let presets = komponist_hosting::discover_clap_presets(bundle)?;
        serde_json::to_writer(out, &presets).map_err(|error| HostingError::State(error.to_string()))
    })
}

/// Loads a catalogued preset into an inactive instance before the app creates a new device.
pub fn run_preset_state(bundle: &Path, preset: &str) -> u8 {
    run_output(bundle, false, |out| {
        use komponist_plugin_api::clap_presets::{ClapPreset, ClapPresetSource};
        let (plugin_id, source): (String, ClapPresetSource) =
            serde_json::from_str(preset).map_err(|error| HostingError::State(error.to_string()))?;
        let bridge = komponist_hosting::host_loop::PollLoop::new(false)?;
        let mut plugin = komponist_hosting::load(
            komponist_ipc::PluginFormat::Clap,
            bundle,
            &plugin_id,
            bridge.context(),
        )?;
        plugin.load_clap_preset(&ClapPreset {
            plugin_id,
            source,
            name: String::new(),
            localized_names: Default::default(),
            features: Vec::new(),
            factory: false,
        })?;
        out.write_all(&plugin.save_state()?)
            .map_err(|error| HostingError::State(error.to_string()))
    })
}

fn run_output(
    bundle: &Path,
    newline: bool,
    write_result: impl FnOnce(&mut File) -> Result<(), HostingError>,
) -> u8 {
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
    match write_result(&mut out) {
        Ok(()) => match (if newline {
            out.write_all(b"\n")
        } else {
            Ok(())
        })
        .and_then(|()| out.flush())
        {
            Ok(()) => exit::SUCCESS,
            Err(e) => {
                eprintln!("komponist-bridge: writing the scan result: {e}");
                exit::FAILURE
            }
        },
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

/// A duplicate of stdout for the JSON; fd 1 itself then points at stderr (Windows: both standard
/// outputs, the process's and the C runtime's, `crate::win_stdio`).
#[cfg(windows)]
fn json_output() -> std::io::Result<File> {
    crate::win_stdio::move_stdout_to_stderr()
}

/// A duplicate of stdout for the JSON; fd 1 itself then points at stderr.
#[cfg(unix)]
fn json_output() -> std::io::Result<File> {
    let out = rustix::io::dup(rustix::stdio::stdout())?;
    rustix::stdio::dup2_stdout(rustix::stdio::stderr())?;
    Ok(File::from(out))
}
