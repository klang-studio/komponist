//! In-process tests of the CLAP layer.
//!
//! - [`fixtures`]: the WP-27 test plugins (`scripts/build-test-plugins.sh` builds them into
//!   `target/test-plugins/<os>/clap/`). Run by default; each test is skipped with a message when its
//!   bundle is missing.
//! - [`installed`]: plugins installed on this machine (Klopfer, Surge XT, Dexed, Shortcircuit XT).
//!   Klopfer runs by default when installed; the third-party plugins are `#[ignore]` (large, slow
//!   to load) and run with `cargo test -p komponist-hosting -- --ignored installed`.
//! - [`gui`]: embedded GUIs. `#[ignore]` and additionally guarded by `KP_GUI_TESTS=1`, so they
//!   never open windows on a desktop by accident. Run them under Xvfb only:
//!
//!   ```text
//!   KP_GUI_TESTS=1 xvfb-run -a -s "-screen 0 1920x1080x24" \
//!       cargo test -p komponist-hosting -- --ignored --test-threads=1 gui
//!   ```

mod fixtures;
#[cfg(x11_embed)]
pub(crate) mod gui;
mod installed;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use assert_no_alloc::assert_no_alloc;
use komponist_ipc::shm::OUTPUT_EVENT_CAPACITY;
use komponist_ipc::{WireEvent, WireTransport};

use crate::host_loop::PollLoop;
use crate::plugin::{ActivateConfig, EventOut, HostedPlugin, HostedProcessor, ProcessBlock};
use crate::{PluginFormat, ProcessStatus};

pub(crate) const SAMPLE_RATE: f64 = 48_000.0;
pub(crate) const BLOCK: usize = 256;

pub(crate) const CONFIG: ActivateConfig = ActivateConfig {
    sample_rate: SAMPLE_RATE,
    min_frames: 1,
    max_frames: BLOCK as u32,
};

/// A fixture bundle, or `None` (with a note on stderr) if it has not been built.
pub(super) fn fixture(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-plugins")
        .join(std::env::consts::OS)
        .join("clap")
        .join(format!("{name}.clap"));
    if path.exists() {
        Some(path)
    } else {
        eprintln!(
            "skipped: {} is missing; run scripts/build-test-plugins.sh",
            path.display()
        );
        None
    }
}

/// An installed bundle (`~/` expanded), or `None` (with a note) if it is not installed.
pub(crate) fn installed(path: &str) -> Option<PathBuf> {
    let path = match path.strip_prefix("~/") {
        Some(rest) => Path::new(&std::env::var("HOME").ok()?).join(rest),
        None => PathBuf::from(path),
    };
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped: {} is not installed", path.display());
        None
    }
}

/// A loaded plugin with the loop serving it. Field order: the plugin goes before its loop.
pub(super) struct Rig {
    pub(super) plugin: Box<dyn HostedPlugin>,
    pub(super) host_loop: Rc<PollLoop>,
}

pub(super) fn load(bundle: &Path, plugin_id: &str) -> Rig {
    let host_loop = PollLoop::new(std::env::var_os("KP_PLUGIN_LOG").is_some()).unwrap();
    let plugin = crate::load(PluginFormat::Clap, bundle, plugin_id, host_loop.context())
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

/// Buffers for processing blocks, allocated up front so that `process` runs inside
/// `assert_no_alloc`.
pub(crate) struct Blocks {
    pub(crate) input: [Vec<f32>; 2],
    /// The sidechain of every block (`None`: none).
    pub(crate) sidechain: Option<[Vec<f32>; 2]>,
    pub(crate) output: [Vec<f32>; 2],
    pub(crate) events_out: Vec<WireEvent>,
    pub(crate) out_count: usize,
    pub(crate) transport: WireTransport,
    pub(crate) steady_time: u64,
}

impl Blocks {
    pub(crate) fn new() -> Blocks {
        Blocks {
            input: [vec![0.0; BLOCK], vec![0.0; BLOCK]],
            sidechain: None,
            output: [vec![0.0; BLOCK], vec![0.0; BLOCK]],
            events_out: vec![WireEvent::default(); OUTPUT_EVENT_CAPACITY],
            out_count: 0,
            transport: WireTransport::default(),
            steady_time: 0,
        }
    }

    /// Processes one block of `BLOCK` frames with `events`; the output stays in `self.output`.
    pub(crate) fn run(
        &mut self,
        processor: &mut dyn HostedProcessor,
        events: &[WireEvent],
    ) -> ProcessStatus {
        // Poison the output: the plugin must write every frame.
        for channel in &mut self.output {
            channel.fill(f32::NAN);
        }
        let [in_l, in_r] = &self.input;
        let [out_l, out_r] = &mut self.output;
        let mut block = ProcessBlock {
            frames: BLOCK as u32,
            input: [in_l, in_r],
            sidechain: self.sidechain.as_ref().map(|[l, r]| [&l[..], &r[..]]),
            output: [out_l, out_r],
            events_in: events,
            events_out: EventOut::new(&mut self.events_out),
            transport: &self.transport,
            steady_time: self.steady_time,
        };
        let status = assert_no_alloc(|| processor.process(&mut block));
        self.out_count = block.events_out.as_slice().len();
        self.steady_time += BLOCK as u64;
        status
    }

    /// Largest absolute output sample of both channels in `range` of the last block.
    pub(crate) fn peak(&self, range: std::ops::Range<usize>) -> f32 {
        self.output
            .iter()
            .flat_map(|c| c[range.clone()].iter())
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Whether the last block contains a NaN (a frame the plugin did not write).
    pub(crate) fn has_nan(&self) -> bool {
        self.output.iter().flatten().any(|s| s.is_nan())
    }
}

/// The `kp-test-sc` checks for either format (#6): the loaded plugin reports its sidechain port,
/// a block's sidechain reaches it sample-exactly (the plugin outputs input + sidechain), and a
/// block without one feeds it silence (the output is the input).
pub(crate) fn sidechain_reaches_the_port(plugin: &mut dyn HostedPlugin) {
    assert!(plugin.descriptor().sidechain, "{:?}", plugin.descriptor());
    let mut processor = plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    let wave = |step: f32, phase: f32| -> Vec<f32> {
        (0..BLOCK)
            .map(|i| (i as f32 * step + phase).sin() * 0.5)
            .collect()
    };
    let mut blocks = Blocks::new();
    for block in 0..3 {
        let phase = block as f32;
        blocks.input = [wave(0.01, phase), wave(0.02, phase)];
        blocks.sidechain = Some([wave(0.03, phase), wave(-0.05, phase)]);
        assert_eq!(blocks.run(processor.as_mut(), &[]), ProcessStatus::Continue);
        let sidechain = blocks.sidechain.as_ref().unwrap();
        for (ch, (input, side)) in blocks.input.iter().zip(sidechain).enumerate() {
            let sum: Vec<f32> = input.iter().zip(side).map(|(a, b)| a + b).collect();
            assert_eq!(blocks.output[ch], sum, "block {block}, channel {ch}");
        }
    }
    // No sidechain: the port is silent again, nothing of the last block stays.
    blocks.sidechain = None;
    assert_eq!(blocks.run(processor.as_mut(), &[]), ProcessStatus::Continue);
    assert_eq!(blocks.output, blocks.input);
    processor.stop_processing();
    plugin.deactivate(processor);
}
