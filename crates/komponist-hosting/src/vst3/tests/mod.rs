//! In-process tests of the VST3 layer, laid out like the CLAP layer's (`crate::clap::tests`, whose
//! block helpers they share).
//!
//! - [`fixtures`]: the WP-27 test plugins as VST3 (`scripts/build-test-plugins.sh` builds them into
//!   `target/test-plugins/<os>/vst3/`). Run by default; each test is skipped with a message when its
//!   bundle is missing.
//! - [`installed`]: VST3 plugins installed on this machine. Klopfer runs by default when installed;
//!   the third-party plugins (Surge XT, Surge XT Effects, Dexed, DecentSampler, Shortcircuit XT)
//!   are `#[ignore]`: `cargo test -p komponist-hosting -- --ignored vst3::tests::installed`.
//! - [`gui`]: embedded editors. `#[ignore]` and guarded by `KP_GUI_TESTS=1`; run under Xvfb only:
//!
//!   ```text
//!   KP_GUI_TESTS=1 xvfb-run -a -s "-screen 0 1920x1080x24" \
//!       cargo test -p komponist-hosting -- --ignored --test-threads=1 vst3::tests::gui
//!   ```
//!
//! Crash isolation (`kp-test-crash`) is tested through the bridge binary only
//! (`komponist-bridge/tests/it/instance.rs`), never in this process.

mod fixtures;
#[cfg(x11_embed)]
mod gui;
mod installed;

use crate::test_fixtures;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::host_loop::PollLoop;
use crate::plugin::HostedPlugin;
use crate::{PluginFormat, ProcessStatus};

pub(super) use crate::clap::tests::{BLOCK, Blocks, CONFIG, SAMPLE_RATE, installed};

/// A VST3 fixture bundle, or `None` (with a note on stderr) if it has not been built.
pub(super) fn fixture(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-plugins")
        .join(std::env::consts::OS)
        .join("vst3")
        .join(format!("{name}.vst3"));
    if test_fixtures::available(&path) {
        Some(path)
    } else {
        None
    }
}

/// A fixture's class id as komponist's plugin id (its 16 ASCII bytes in hex).
pub(super) fn fixture_id(class_id: &[u8; 16]) -> String {
    class_id.iter().map(|b| format!("{b:02X}")).collect()
}

/// A loaded plugin with the loop serving it. Field order: the plugin goes before its loop.
pub(super) struct Rig {
    pub(super) plugin: Box<dyn HostedPlugin>,
    pub(super) host_loop: Rc<PollLoop>,
}

pub(super) fn load(bundle: &Path, plugin_id: &str) -> Rig {
    let host_loop = PollLoop::new(std::env::var_os("KP_PLUGIN_LOG").is_some()).unwrap();
    let plugin = crate::load(PluginFormat::Vst3, bundle, plugin_id, host_loop.context())
        .unwrap_or_else(|e| panic!("loading {plugin_id} from {}: {e}", bundle.display()));
    Rig { plugin, host_loop }
}

/// Loads the first plugin of a bundle.
#[cfg(x11_embed)]
pub(super) fn load_first(bundle: &Path) -> Rig {
    let descriptors = crate::scan(bundle).unwrap();
    let first = descriptors.first().expect("no plugin in the bundle");
    load(bundle, &first.id)
}

/// The id of the parameter called `name`.
pub(super) fn param_id(plugin: &dyn HostedPlugin, name: &str) -> u32 {
    plugin
        .params()
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no parameter {name}"))
        .id
}

/// Processes `blocks` empty blocks; the largest peak seen.
pub(super) fn run_silent(
    blocks: &mut Blocks,
    processor: &mut dyn crate::HostedProcessor,
    count: usize,
) -> f32 {
    let mut peak = 0.0f32;
    for _ in 0..count {
        assert_ne!(blocks.run(processor, &[]), ProcessStatus::Error);
        peak = peak.max(blocks.peak(0..BLOCK));
    }
    peak
}
