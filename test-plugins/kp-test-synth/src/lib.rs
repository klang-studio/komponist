//! kp-test-synth: the instrument fixture for komponist's plugin hosting tests (PLAN WP-27).
//!
//! One sine voice per note (velocity sets the level) that falls by 60 dB within the `decay` time
//! while held and within 10 ms after the note-off; the voices' sum, scaled by `gain`, goes to both
//! output channels. The state is the parameters. A minimal egui editor (one slider, one label)
//! serves GUI embedding tests without klopfer.

mod editor;
pub mod voices;

use nice_plug::prelude::*;
use nice_plug_egui::EguiEditorState;
use std::sync::Arc;
use voices::Voices;

pub struct KpTestSynth {
    params: Arc<SynthParams>,
    editor_state: Arc<EguiEditorState>,
    voices: Voices,
}

/// Parameter IDs are part of saved states and must never change.
#[derive(Params)]
pub struct SynthParams {
    /// Output level in dB.
    #[id = "gain"]
    pub gain: FloatParam,
    /// Time a held note takes to fall by 60 dB, in seconds.
    #[id = "decay"]
    pub decay: FloatParam,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self {
            // No smoothers: a parameter change applies exactly at the next block, which keeps the
            // output predictable for host tests.
            gain: FloatParam::new(
                "Gain",
                -12.0,
                FloatRange::Linear {
                    min: -60.0,
                    max: 0.0,
                },
            )
            .with_step_size(0.1)
            .with_unit(" dB"),
            decay: FloatParam::new(
                "Decay",
                1.0,
                FloatRange::Skewed {
                    min: 0.01,
                    max: 10.0,
                    factor: FloatRange::skew_factor(-2.0),
                },
            )
            .with_unit(" s")
            .with_value_to_string(formatters::v2s_f32_rounded(2)),
        }
    }
}

impl Default for KpTestSynth {
    fn default() -> Self {
        Self {
            params: Arc::new(SynthParams::default()),
            editor_state: editor::default_state(),
            voices: Voices::new(48_000.0),
        }
    }
}

impl KpTestSynth {
    pub fn params(&self) -> &SynthParams {
        &self.params
    }

    fn handle(&mut self, event: NoteEvent<()>) {
        match event {
            NoteEvent::NoteOn { key, velocity, .. } => {
                if let Some(key) = key.number() {
                    self.voices.note_on(key, velocity);
                }
            }
            NoteEvent::NoteOff { key, .. } => match key.number() {
                Some(key) => self.voices.note_off(key),
                None => self.voices.release_all(),
            },
            NoteEvent::Choke { .. } => self.voices.reset(),
            _ => (),
        }
    }
}

impl Plugin for KpTestSynth {
    const NAME: &'static str = "kp-test-synth";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio";
    const EMAIL: &'static str = "info@example.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: None,
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::Basic;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type Editor = nice_plug_egui::EguiEditor<editor::SynthEditor>;
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Self::Editor> {
        editor::create(self.editor_state.clone(), self.params.clone())
    }

    fn activate(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl ActivateContext<Self>,
    ) -> bool {
        self.voices = Voices::new(buffer_config.sample_rate);
        true
    }

    fn reset(&mut self) {
        self.voices.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        self.voices.set_decay(self.params.decay.value());
        let gain = util::db_to_gain(self.params.gain.value());

        let mut next_event = context.next_event();
        for (index, frame) in buffer.iter_samples().enumerate() {
            while let Some(event) = next_event.filter(|e| e.timing() <= index as u32) {
                self.handle(event);
                next_event = context.next_event();
            }
            let sample = self.voices.next_sample() * gain;
            for out in frame {
                *out = sample;
            }
        }
        // Events past the end of the block (hosts should not send any) still count.
        while let Some(event) = next_event {
            self.handle(event);
            next_event = context.next_event();
        }

        if self.voices.active() == 0 {
            ProcessStatus::Normal
        } else {
            ProcessStatus::KeepAlive
        }
    }
}

impl ClapPlugin for KpTestSynth {
    const CLAP_ID: &'static str = "de.klang-studio.kp-test-synth";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("komponist test instrument: a sine per note with gain and decay");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Synthesizer,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for KpTestSynth {
    const VST3_CLASS_ID: [u8; 16] = *b"kpTestSynth00001";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Instrument, Vst3SubCategory::Synth];
}

nice_export_clap!(KpTestSynth);
nice_export_vst3!(KpTestSynth);

#[cfg(test)]
mod tests {
    use super::*;
    use fake_host::{FakeHost, note_off, note_on};

    fn silence(samples: usize) -> Vec<Vec<f32>> {
        vec![vec![0.0; samples]; 2]
    }

    fn peak(channel: &[f32]) -> f32 {
        channel.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn silent_without_notes() {
        let mut host = FakeHost::<KpTestSynth>::activate();
        let mut block = silence(256);
        assert_eq!(host.process(&mut block, &[]), ProcessStatus::Normal);
        assert!(block.iter().all(|c| c.iter().all(|&s| s == 0.0)));
    }

    #[test]
    fn note_starts_at_its_sample_offset_on_both_channels() {
        let mut host = FakeHost::<KpTestSynth>::activate();
        let mut block = silence(256);
        let status = host.process(&mut block, &[note_on(100, 69)]);
        assert_eq!(status, ProcessStatus::KeepAlive);
        assert!(block[0][..=100].iter().all(|&s| s == 0.0));
        assert!(block[0][101] != 0.0);
        assert_eq!(block[0], block[1]);
    }

    #[test]
    fn gain_scales_the_output() {
        let mut host = FakeHost::<KpTestSynth>::activate();
        let gain = &host.plugin.params().gain;
        // 0 dB is the top of the range.
        host.set_param(gain, 1.0);
        assert_eq!(gain.value(), 0.0);

        let mut block = silence(1024);
        host.process(&mut block, &[note_on(0, 69)]);
        let loud = peak(&block[0]);
        assert!(loud > 0.95, "{loud}");

        host.set_param(&host.plugin.params().gain, 0.0);
        let mut block = silence(1024);
        host.process(&mut block, &[note_on(0, 69)]);
        let quiet = peak(&block[0]);
        assert!((quiet / loud - 0.001).abs() < 1e-4, "{quiet} vs {loud}");
    }

    #[test]
    fn note_off_ends_the_note() {
        let mut host = FakeHost::<KpTestSynth>::activate();
        let mut block = silence(1024);
        host.process(&mut block, &[note_on(0, 60), note_off(10, 60)]);
        let mut block = silence(1024);
        assert_eq!(host.process(&mut block, &[]), ProcessStatus::Normal);
        assert!(peak(&block[0]) == 0.0);
    }

    #[test]
    fn state_is_the_params() {
        let synth = KpTestSynth::default();
        let ids: Vec<String> = synth.params.param_map().into_iter().map(|p| p.0).collect();
        assert_eq!(ids, ["gain", "decay"]);
        assert!(synth.params.serialize_fields().is_empty());
    }
}
