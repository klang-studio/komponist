//! A fake host for the test plugins' unit tests.
//!
//! It calls a plugin's [`Plugin`] impl the way nice-plug's CLAP and VST3 wrappers do (activate,
//! reset, process with note events), but in-process and without the FFI layer. `process` runs
//! inside `assert_no_alloc`: nice-plug's `assert_process_allocs` feature installs the allocator
//! that aborts on allocations in debug builds.

use nice_plug::context::process::SendEventError;
use nice_plug::midi::{Channel, Key, VoiceID};
use nice_plug::prelude::*;
use std::cell::Cell;

/// The sample rate the fake host activates plugins with.
pub const SAMPLE_RATE: f32 = 48_000.0;
/// The largest block the fake host promises in `activate`.
pub const MAX_BLOCK: usize = 1024;

/// A plugin instance, activated with the first of its audio IO layouts.
pub struct FakeHost<P: Plugin> {
    /// The plugin, for direct access to its params.
    pub plugin: P,
    latency: Cell<u32>,
    transport: Transport,
}

impl<P: Plugin> FakeHost<P> {
    /// Creates the plugin with `P::default()`, activates and resets it.
    pub fn activate() -> Self {
        Self::activate_with(P::default())
    }

    /// Activates and resets `plugin`.
    pub fn activate_with(mut plugin: P) -> Self {
        let latency = Cell::new(0);
        let layout = P::AUDIO_IO_LAYOUTS.first().copied().unwrap_or_default();
        let config = BufferConfig {
            sample_rate: SAMPLE_RATE,
            min_buffer_size: None,
            max_buffer_size: MAX_BLOCK as u32,
            process_mode: ProcessMode::Realtime,
        };
        let activated = plugin.activate(&layout, &config, &mut Activate { latency: &latency });
        assert!(activated, "{} refused to activate", P::NAME);
        plugin.reset();
        Self {
            plugin,
            latency,
            transport: Transport::new(SAMPLE_RATE),
        }
    }

    /// The latency the plugin reported last, in samples.
    pub fn latency(&self) -> u32 {
        self.latency.get()
    }

    /// Sets a parameter to a normalized value, like a host's parameter change does.
    pub fn set_param(&self, param: &impl Param, normalized: f32) {
        let ptr = param.as_ptr();
        // SAFETY: `ptr` points into the plugin's params, which outlive this call; the fake host
        // is single threaded, so nothing reads the parameter concurrently.
        unsafe {
            ptr._internal_set_normalized_value(normalized);
            ptr._internal_update_smoother(SAMPLE_RATE, false);
        }
    }

    /// Processes one block in place. All channels must have the same length (at most
    /// [`MAX_BLOCK`]); event timings are sample offsets into the block.
    pub fn process(
        &mut self,
        channels: &mut [Vec<f32>],
        events: &[PluginNoteEvent<P>],
    ) -> ProcessStatus {
        self.process_aux(channels, None, events)
    }

    /// [`Self::process`] with `sidechain` as the first auxiliary input (same length as the main
    /// channels).
    pub fn process_with_sidechain(
        &mut self,
        channels: &mut [Vec<f32>],
        sidechain: &mut [Vec<f32>],
        events: &[PluginNoteEvent<P>],
    ) -> ProcessStatus {
        self.process_aux(channels, Some(sidechain), events)
    }

    fn process_aux(
        &mut self,
        channels: &mut [Vec<f32>],
        sidechain: Option<&mut [Vec<f32>]>,
        events: &[PluginNoteEvent<P>],
    ) -> ProcessStatus {
        let samples = channels.first().map_or(0, Vec::len);
        assert!(samples <= MAX_BLOCK, "block longer than MAX_BLOCK");
        assert!(
            channels
                .iter()
                .chain(sidechain.iter().flat_map(|s| s.iter()))
                .all(|c| c.len() == samples),
            "channels differ in length"
        );

        let mut buffer = Buffer::default();
        // SAFETY: the slices borrow `channels` for the lifetime of `buffer`, all of equal length.
        unsafe {
            buffer.set_slices(samples, |slices| {
                slices.extend(channels.iter_mut().map(Vec::as_mut_slice));
            });
        }
        let mut inputs: Vec<Buffer> = Vec::new();
        if let Some(sidechain) = sidechain {
            let mut side = Buffer::default();
            // SAFETY: as above, for `sidechain`.
            unsafe {
                side.set_slices(samples, |slices| {
                    slices.extend(sidechain.iter_mut().map(Vec::as_mut_slice));
                });
            }
            inputs.push(side);
        }
        let mut aux = AuxiliaryBuffers {
            inputs: &mut inputs,
            outputs: &mut [],
        };
        let mut context = Process::<P> {
            events: events.iter(),
            transport: &self.transport,
            latency: &self.latency,
        };
        let plugin = &mut self.plugin;
        nice_assert_no_alloc::assert_no_alloc(|| {
            plugin.process(&mut buffer, &mut aux, &mut context)
        })
    }
}

/// A note-on with full velocity on channel 0.
pub fn note_on<S>(timing: u32, key: u8) -> NoteEvent<S> {
    NoteEvent::NoteOn {
        timing,
        voice_id: VoiceID::Wildcard,
        channel: Channel::Number(0),
        key: Key::Number(key),
        velocity: 1.0,
    }
}

/// A note-off on channel 0.
pub fn note_off<S>(timing: u32, key: u8) -> NoteEvent<S> {
    NoteEvent::NoteOff {
        timing,
        voice_id: VoiceID::Wildcard,
        channel: Channel::Number(0),
        key: Key::Number(key),
        velocity: 0.0,
    }
}

struct Activate<'a> {
    latency: &'a Cell<u32>,
}

impl<P: Plugin> ActivateContext<P> for Activate<'_> {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }

    fn execute(&self, _task: P::BackgroundTask) {}

    fn set_latency_samples(&self, samples: u32) {
        self.latency.set(samples);
    }

    fn set_current_voice_capacity(&self, _capacity: u32) {}
}

struct Process<'a, P: Plugin> {
    events: std::slice::Iter<'a, PluginNoteEvent<P>>,
    transport: &'a Transport,
    latency: &'a Cell<u32>,
}

impl<P: Plugin> ProcessContext<P> for Process<'_, P> {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }

    fn execute_background(&self, _task: P::BackgroundTask) {}

    fn execute_gui(&self, _task: P::BackgroundTask) {}

    fn transport(&self) -> &Transport {
        self.transport
    }

    fn next_event(&mut self) -> Option<PluginNoteEvent<P>> {
        self.events.next().cloned()
    }

    fn try_send_event(
        &mut self,
        event: PluginNoteEvent<P>,
    ) -> Result<(), (PluginNoteEvent<P>, SendEventError)> {
        Err((event, SendEventError::NoOutputBuffer))
    }

    fn set_latency_samples(&self, samples: u32) {
        self.latency.set(samples);
    }

    fn request_restart(&self) {}

    fn set_current_voice_capacity(&self, _capacity: u32) {}
}
