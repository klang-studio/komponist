//! kp-test-slow: the deadline-miss fixture for komponist's bridge (PLAN WP-27).
//!
//! A stereo passthrough with a note input that sleeps [`STALL`] inside `process` for every
//! note-on of key [`SLOW_KEY`]. That is longer than a block at usual sizes (2048 samples at
//! 48 kHz last 42.7 ms), so the engine's deadline passes.

use nice_plug::midi::Key;
use nice_plug::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A note-on of this key stalls `process`.
pub const SLOW_KEY: u8 = 126;
/// How long `process` sleeps per note-on of [`SLOW_KEY`].
pub const STALL: Duration = Duration::from_millis(50);
/// A note-on of this key delays the next state snapshot on the bridge control thread.
pub const SLOW_STATE_KEY: u8 = 125;
/// A note-on of this key makes the next state snapshot change `gain` while it serializes.
pub const MUTATE_STATE_KEY: u8 = 124;
/// A note-on of this key makes `gain` move every block while a state snapshot is being saved
/// (and return to 0.5 afterwards), like automation would. It stays on.
pub const DRIFT_KEY: u8 = 123;
/// Delay for the state snapshot, long enough to exercise the project's shared timeout budget.
pub const STATE_STALL: Duration = Duration::from_millis(600);

#[derive(Default)]
pub struct KpTestSlow {
    params: Arc<SlowParams>,
    blocks: u32,
}

/// One unused `gain` parameter. The audio thread can arm a one-shot stall for the next state
/// request and a one-shot change of `gain` during that request.
pub struct SlowParams {
    /// Does not affect the audio; tests read and change it.
    pub gain: FloatParam,
    delay_state: AtomicBool,
    mutate_state: AtomicBool,
    saving: AtomicBool,
    drift: AtomicBool,
}

impl Default for SlowParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new("Gain", 0.5, FloatRange::Linear { min: 0.0, max: 1.0 }),
            delay_state: AtomicBool::new(false),
            mutate_state: AtomicBool::new(false),
            saving: AtomicBool::new(false),
            drift: AtomicBool::new(false),
        }
    }
}

// SAFETY: the only pointer is `gain`, which lives as long as the params.
unsafe impl Params for SlowParams {
    fn param_map(&self) -> Vec<(String, ParamPtr, String)> {
        vec![("gain".to_owned(), self.gain.as_ptr(), String::new())]
    }

    fn serialize_fields(&self) -> BTreeMap<String, String> {
        if self.delay_state.swap(false, Ordering::Relaxed) {
            self.saving.store(true, Ordering::Relaxed);
            std::thread::sleep(STATE_STALL);
            self.saving.store(false, Ordering::Relaxed);
        }
        if self.mutate_state.swap(false, Ordering::Relaxed) {
            // SAFETY: the pointer is valid; the value is atomic.
            unsafe { self.gain.as_ptr()._internal_set_normalized_value(0.75) };
        }
        BTreeMap::new()
    }
}

impl Plugin for KpTestSlow {
    const NAME: &'static str = "kp-test-slow";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio";
    const EMAIL: &'static str = "info@example.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::Basic;

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn process(
        &mut self,
        _buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        while let Some(event) = context.next_event() {
            if let NoteEvent::NoteOn {
                key: Key::Number(key),
                ..
            } = event
            {
                match key {
                    // Breaks the realtime rules on purpose.
                    SLOW_KEY => std::thread::sleep(STALL),
                    SLOW_STATE_KEY => self.params.delay_state.store(true, Ordering::Relaxed),
                    DRIFT_KEY => self.params.drift.store(true, Ordering::Relaxed),
                    MUTATE_STATE_KEY => self.params.mutate_state.store(true, Ordering::Relaxed),
                    _ => {}
                }
            }
        }
        if self.params.drift.load(Ordering::Relaxed) {
            self.blocks = self.blocks.wrapping_add(1);
            let value = if !self.params.saving.load(Ordering::Relaxed) {
                0.5
            } else if self.blocks % 2 == 0 {
                0.2
            } else {
                0.8
            };
            // SAFETY: the pointer is valid; the value is atomic.
            unsafe { self.params.gain.as_ptr()._internal_set_normalized_value(value) };
        }
        // The audio passes through: the wrapper has copied the input to the output.
        ProcessStatus::Normal
    }
}

impl ClapPlugin for KpTestSlow {
    const CLAP_ID: &'static str = "de.klang-studio.kp-test-slow";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("komponist test plugin: sleeps 50 ms inside process on note 126");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Utility,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for KpTestSlow {
    const VST3_CLASS_ID: [u8; 16] = *b"kpTestSlow000001";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Tools];
}

nice_export_clap!(KpTestSlow);
nice_export_vst3!(KpTestSlow);

#[cfg(test)]
mod tests {
    use super::*;
    use fake_host::{FakeHost, note_on};
    use std::time::Instant;

    fn block() -> Vec<Vec<f32>> {
        vec![vec![0.25; 64], vec![-0.25; 64]]
    }

    fn timed(host: &mut FakeHost<KpTestSlow>, events: &[NoteEvent<()>]) -> Duration {
        let start = Instant::now();
        host.process(&mut block(), events);
        start.elapsed()
    }

    #[test]
    fn note_126_stalls_process_per_note_on() {
        let mut host = FakeHost::<KpTestSlow>::activate();
        assert!(timed(&mut host, &[note_on(0, SLOW_KEY)]) >= STALL);
        let two = [note_on(0, SLOW_KEY), note_on(5, SLOW_KEY)];
        assert!(timed(&mut host, &two) >= 2 * STALL);
    }

    #[test]
    fn other_notes_do_not_stall() {
        let mut host = FakeHost::<KpTestSlow>::activate();
        assert!(timed(&mut host, &[note_on(0, 60), note_on(1, 127)]) < STALL / 2);
    }

    #[test]
    fn passes_audio_through() {
        let mut host = FakeHost::<KpTestSlow>::activate();
        let mut audio = block();
        host.process(&mut audio, &[note_on(0, SLOW_KEY)]);
        assert_eq!(audio, block());
    }
}
