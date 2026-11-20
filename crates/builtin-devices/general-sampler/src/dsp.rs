//! Realtime adapter for the chromatic sampler. The only sample ownership
//! changes on the audio thread are transfers through the loader's queues.
use std::sync::Arc;

use sampler_core::{
    Engine, EnvParams, FilterMode, LfoShape, ModSlot, ModSource, ModTarget, NUM_OUTPUTS, PadParams,
    PadSettings, PlaybackDirection, PlaybackRegion, Sample, SampleTrigger,
};

use komponist_device_core::Dsp;
use komponist_sampler::load::{self, AudioSide, Meters, SamplerHandle, SlicePreview};

use super::{MAX_SLICES, PARAM_COUNT, Param, Slice, SliceOptions};

const SOURCE: usize = 0;
const STRETCH_SLOTS: [usize; 2] = [1, 2];
const PAD: usize = 0;
// Incoming notes with this sentinel are translated to None by InternalProcessor.
const SLICE_PREVIEW_KEY: u8 = 0;
const SLICE_PREVIEW_ID: u32 = komponist_engine::NOTE_ID_NONE;

pub(super) struct GeneralSamplerDsp {
    engine: Box<Engine>,
    values: [f64; PARAM_COUNT],
    settings: [PadSettings; 1],
    sample_rate: f32,
    project_bpm: f64,
    samples: [Option<Arc<Sample>>; 3],
    generation: u64,
    stretched_ratios: [u32; 3],
    newest_stretch_slot: usize,
    slice_revision: u64,
    slices: [Slice; MAX_SLICES + 1],
    handle: SamplerHandle,
    audio: AudioSide,
    meters: Meters,
}

impl GeneralSamplerDsp {
    pub(super) fn new() -> Self {
        let (handle, audio, meters) = load::general_channel();
        let sample_rate = 48_000.0;
        Self {
            engine: Box::new(Engine::new(sample_rate)),
            values: [0.0; PARAM_COUNT],
            settings: [PadSettings::default()],
            sample_rate,
            project_bpm: 120.0,
            samples: [None, None, None],
            generation: 0,
            stretched_ratios: [0; 3],
            newest_stretch_slot: STRETCH_SLOTS[0],
            slice_revision: 0,
            slices: [Slice::default(); MAX_SLICES + 1],
            handle,
            audio,
            meters,
        }
    }

    pub(super) fn handle(&self) -> SamplerHandle {
        self.handle.clone()
    }

    fn value(&self, param: Param) -> f64 {
        self.values[param as usize]
    }

    fn requested_ratio(&self) -> u32 {
        load::sync_ratio(
            self.value(Param::SourceBpm),
            self.project_bpm,
            self.value(Param::Sync) >= 0.5,
        )
    }

    fn refresh_sync(&self) {
        self.meters.request_stretch(self.requested_ratio());
    }

    fn stretch_slot(&self, ratio: u32) -> Option<usize> {
        STRETCH_SLOTS
            .into_iter()
            .find(|&slot| self.stretched_ratios[slot] == ratio)
    }

    fn update_settings(&mut self) {
        let value = |param| self.value(param) as f32;
        let mut params = PadParams {
            volume_db: value(Param::Gain),
            mute: value(Param::Gain) <= -70.0,
            pan: value(Param::Pan),
            tune: value(Param::Coarse) + value(Param::Fine) / 100.0,
            gate: value(Param::Mode) == 1.0,
            amp: EnvParams {
                attack_ms: value(Param::AmpAttack),
                hold_ms: 0.0,
                decay_ms: value(Param::AmpDecay),
                sustain: value(Param::AmpSustain),
                release_ms: value(Param::AmpRelease),
            },
            filter_mode: match value(Param::Filter) as u8 {
                1 => FilterMode::LowPass,
                2 => FilterMode::HighPass,
                3 => FilterMode::BandPass,
                _ => FilterMode::Off,
            },
            cutoff_hz: value(Param::Cutoff),
            resonance: value(Param::Resonance),
            filter_env_octaves: value(Param::FilterEnv),
            filter_env_decay_ms: value(Param::FilterDecay),
            lfo_shape: match value(Param::LfoShape) as u8 {
                1 => LfoShape::Triangle,
                2 => LfoShape::SawUp,
                3 => LfoShape::SawDown,
                4 => LfoShape::Square,
                5 => LfoShape::SampleHold,
                _ => LfoShape::Sine,
            },
            lfo_rate_hz: value(Param::LfoRate),
            lfo_retrigger: false,
            ..PadParams::default()
        };
        let target = |param| match value(param) as u8 {
            1 => ModTarget::Pitch,
            2 => ModTarget::Cutoff,
            3 => ModTarget::SampleStart,
            _ => ModTarget::Off,
        };
        params.slots[0] = ModSlot {
            source: ModSource::Lfo,
            target: target(Param::LfoTarget),
            amount: value(Param::LfoDepth),
        };
        params.slots[1] = ModSlot {
            source: ModSource::Velocity,
            target: target(Param::VelocityTarget),
            amount: value(Param::VelocityDepth),
        };
        self.settings[PAD] = PadSettings::new(&params, self.sample_rate);
    }

    /// Called only with room for the outgoing sample in the retire queue.
    fn replace_sample(&mut self, slot: usize, sample: Option<Arc<Sample>>) {
        self.engine.kill_slot(slot);
        let previous = std::mem::replace(&mut self.samples[slot], sample);
        let _ = self.audio.garbage.push(previous);
    }

    fn take_loaded(&mut self) {
        self.meters
            .read_slices(&mut self.slice_revision, &mut self.slices);
        while self.audio.garbage.slots() >= self.samples.len() {
            let Ok(loaded) = self.audio.incoming.pop() else {
                break;
            };
            if loaded.pad == PAD && self.meters.is_current(PAD, loaded.generation) {
                self.generation = loaded.generation;
                self.replace_sample(SOURCE, loaded.sample);
                for slot in STRETCH_SLOTS {
                    self.replace_sample(slot, None);
                    self.stretched_ratios[slot] = 0;
                }
            } else {
                let _ = self.audio.garbage.push(loaded.sample);
            }
        }
        while self.audio.garbage.slots() > 0 {
            let Some(queue) = self.audio.stretched.as_mut() else {
                break;
            };
            // Keep a current result queued until its source has room to install.
            if queue.peek().is_ok_and(|loaded| {
                loaded.generation != self.generation
                    && self.meters.is_current(PAD, loaded.generation)
            }) {
                break;
            }
            let Ok(loaded) = queue.pop() else {
                break;
            };
            let Some(sample) = loaded.sample else {
                continue;
            };
            if self.generation != loaded.generation
                || !self.meters.is_current(PAD, loaded.generation)
                || self.stretch_slot(loaded.ratio).is_some()
            {
                let _ = self.audio.garbage.push(Some(sample));
                continue;
            }

            let empty = STRETCH_SLOTS
                .into_iter()
                .find(|&slot| self.samples[slot].is_none());
            let slot = if let Some(slot) = empty {
                slot
            } else if loaded.ratio == self.meters.stretch_ratio() {
                // Keep the last tempo's held voices playing. If both prepared
                // buffers still have voices, the oldest yields to the new tempo.
                let heads = self.engine.playheads(&self.samples[..]);
                STRETCH_SLOTS
                    .into_iter()
                    .find(|&slot| heads[slot] < 0.0)
                    .unwrap_or_else(|| {
                        if self.newest_stretch_slot == STRETCH_SLOTS[0] {
                            STRETCH_SLOTS[1]
                        } else {
                            STRETCH_SLOTS[0]
                        }
                    })
            } else {
                let _ = self.audio.garbage.push(Some(sample));
                continue;
            };
            self.replace_sample(slot, Some(sample));
            self.stretched_ratios[slot] = loaded.ratio;
            self.newest_stretch_slot = slot;
        }
    }

    fn play_note(&mut self, key: u8, velocity: u8, note_id: Option<u32>, slice_time: Option<f64>) {
        let Some(source) = self.samples[SOURCE].as_deref() else {
            return;
        };
        // The UI can warm the next tempo before the RT state changes.
        // Trigger against the tempo/params of this block, not that future request.
        let slot = self.stretch_slot(self.requested_ratio()).unwrap_or(SOURCE);
        let sample = self.samples[slot].as_deref().expect("selected sample");
        let length = sample.len() as f64;
        let start = self.value(Param::Start) * length;
        let end = self.value(Param::End) * length;
        if end - start < 1.0 {
            return;
        }
        let root = self.value(Param::Root) as u8;
        let is_slice = self.value(Param::Mode) == 2.0;
        let (start, end, tune, options) = if is_slice {
            let index = if slice_time.is_some() {
                0
            } else if let Some(index) = key.checked_sub(root) {
                usize::from(index)
            } else {
                return;
            };
            let mut first = start;
            let mut remaining = index;
            let frames_per_second = length * f64::from(source.sample_rate) / source.len() as f64;
            let preview_frame = slice_time.map(|time| time * frames_per_second);
            if preview_frame.is_some_and(|frame| !(start..end).contains(&frame)) {
                return;
            }
            let mut last = end;
            let mut options = self.slices[0].options;
            for slice in &self.slices[1..] {
                if slice.time == 0.0 {
                    break;
                }
                let frame = slice.time * frames_per_second;
                if frame <= start {
                    options = slice.options;
                    continue;
                }
                if frame >= end {
                    break;
                }
                if preview_frame.map_or(remaining == 0, |target| target < frame) {
                    last = frame;
                    break;
                }
                remaining = remaining.saturating_sub(1);
                first = frame;
                options = slice.options;
            }
            if remaining != 0 {
                return;
            }
            (first, last, 0.0, options)
        } else {
            if slice_time.is_some() {
                return;
            }
            (
                start,
                end,
                f32::from(key) - f32::from(root),
                SliceOptions::default(),
            )
        };
        let fade_frames = |override_ms: Option<f32>, param| {
            override_ms.map_or_else(|| self.value(param), f64::from) * f64::from(sample.sample_rate)
                / 1000.0
        };
        let reverse = options
            .reverse
            .unwrap_or(self.value(Param::Direction) == 1.0);
        let direction = if reverse {
            PlaybackDirection::Reverse
        } else {
            PlaybackDirection::Forward
        };
        let mut settings = self.settings[PAD];
        if is_slice {
            settings.gate = options.gate;
        }
        self.engine.trigger(
            SampleTrigger {
                pad: PAD,
                slot,
                key,
                note_id,
                velocity: f32::from(velocity.clamp(1, 127)) / 127.0,
                tune,
                independent_pitch: self.value(Param::Sync) >= 0.5,
                region: PlaybackRegion {
                    start,
                    end,
                    fade_in: fade_frames(options.fade_in_ms, Param::FadeIn),
                    fade_out: fade_frames(options.fade_out_ms, Param::FadeOut),
                    looping: None,
                    direction,
                },
            },
            sample,
            &settings,
        );
        self.meters.hit(PAD);
    }
}

impl Dsp for GeneralSamplerDsp {
    fn activate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        *self.engine = Engine::new(sample_rate);
        self.update_settings();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        if let Some(target) = self.values.get_mut(index)
            && value.is_finite()
        {
            *target = value;
            if index == Param::Sync as usize || index == Param::SourceBpm as usize {
                self.refresh_sync();
            }
            self.update_settings();
        }
    }

    fn set_tempo(&mut self, bpm: f64) {
        if self.project_bpm != bpm {
            self.project_bpm = bpm;
            self.refresh_sync();
        }
    }

    fn process(&mut self, _input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        self.take_loaded();
        let released = match self.meters.take_slice_preview() {
            Some(SlicePreview::Start { time, released }) => {
                self.engine.choke_note(SLICE_PREVIEW_ID);
                self.play_note(SLICE_PREVIEW_KEY, 100, Some(SLICE_PREVIEW_ID), Some(time));
                released
            }
            Some(SlicePreview::Release) => true,
            None => false,
        };
        if released {
            self.engine
                .release_note(SLICE_PREVIEW_KEY, Some(SLICE_PREVIEW_ID));
        }
        let [left, right] = output;
        let mut buses = [[0.0; 2]; NUM_OUTPUTS];
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            buses[0] = [0.0; 2];
            self.engine
                .render_frame(&self.samples[..], &self.settings, &mut buses);
            [*l, *r] = buses[0];
        }
        self.engine.advance_lfo_phases(&self.settings, left.len());
        let heads = self.engine.playheads(&self.samples[..]);
        let desired = self.meters.stretch_ratio();
        let current = self.stretch_slot(desired);
        let rendering = self.stretch_slot(self.requested_ratio());
        let active = current
            .into_iter()
            .chain(STRETCH_SLOTS)
            .map(|slot| heads[slot])
            .find(|&head| head >= 0.0)
            .unwrap_or(heads[SOURCE]);
        self.meters.playhead(PAD, active);

        // Release inactive retired tempos outside the audio callback.
        if current.is_some() || desired == load::UNITY_RATIO {
            for slot in STRETCH_SLOTS {
                if Some(slot) != current
                    && Some(slot) != rendering
                    && self.samples[slot].is_some()
                    && heads[slot] < 0.0
                    && self.audio.garbage.slots() > 0
                {
                    self.replace_sample(slot, None);
                    self.stretched_ratios[slot] = 0;
                }
            }
        }
    }

    fn reset(&mut self) {
        let _ = self.meters.take_slice_preview();
        self.engine.reset();
        self.meters.playhead(PAD, -1.0);
    }

    fn is_instrument(&self) -> bool {
        true
    }

    fn note_on_with_id(&mut self, key: u8, velocity: u8, note_id: Option<u32>) {
        // A note at the beginning of the block comes before Dsp::process.
        self.take_loaded();
        self.play_note(key, velocity, note_id, None);
    }

    fn note_off_with_id(&mut self, key: u8, note_id: Option<u32>) {
        self.engine.release_note(key, note_id);
    }

    fn all_notes_off(&mut self) {
        self.engine.choke_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    #[test]
    fn stretch_result_waits_for_its_source_when_retirement_is_backed_up() {
        let mut dsp = GeneralSamplerDsp::new();
        let (mut source, incoming) = rtrb::RingBuffer::new(2);
        let (garbage, mut retired) = rtrb::RingBuffer::new(3);
        let (mut stretch, stretched) = rtrb::RingBuffer::new(2);
        dsp.audio = AudioSide {
            incoming,
            garbage,
            stretched: Some(stretched),
        };
        let generation = 0; // The mock queues use the current generation of the fresh handle.
        dsp.generation = u64::MAX;
        let sample = Arc::new(Sample::mono(48_000.0, vec![0.0; 32]));
        let ratio = 1.5_f32.to_bits();
        source
            .push(load::Loaded {
                pad: PAD,
                generation,
                sample: Some(sample.clone()),
            })
            .ok()
            .unwrap();
        stretch
            .push(load::Stretched {
                generation,
                ratio,
                sample: Some(sample.clone()),
            })
            .ok()
            .unwrap();
        dsp.audio.garbage.push(None).unwrap();
        dsp.audio.garbage.push(None).unwrap();
        assert_no_alloc(|| dsp.take_loaded());
        assert_eq!(dsp.generation, u64::MAX);
        assert!(dsp.audio.stretched.as_ref().unwrap().peek().is_ok());
        for _ in 0..2 {
            assert!(retired.pop().is_ok());
        }
        assert_no_alloc(|| dsp.take_loaded());
        assert_eq!(dsp.generation, generation);
        // Source and both old stretch slots used the available retirement entries.
        for _ in 0..3 {
            assert!(retired.pop().is_ok());
        }
        assert_no_alloc(|| dsp.take_loaded());
        assert!(dsp.stretch_slot(ratio).is_some());
    }
}
