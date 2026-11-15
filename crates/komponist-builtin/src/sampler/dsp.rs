//! The sampler's DSP: `klopfer_dsp`'s engine behind the [`Dsp`] trait of the internal devices.
//!
//! Realtime: `note_on`, `note_off`, `process` and `reset` allocate, lock and free nothing. A sample
//! arrives from the loader as `Arc<Sample>` through a wait-free queue (drained at the start of
//! `process`), the slot's old `Arc` goes back through another one, so the audio thread never drops
//! a sample; a pad's settings are recomputed from plain numbers on a parameter change.

use std::sync::Arc;

use klopfer_dsp::{
    Engine, EnvParams, NUM_OUTPUTS, NUM_PADS, NUM_SAMPLE_SLOTS, PadLayers, PadParams, PadSettings,
    Sample, slot_index,
};

use super::load::{self, AudioSide, Meters, SamplerHandle};
use super::{CHOKE_GROUPS, FOLLOW, KIT_TUNE, MAX_DECAY_S, MAX_MS, PADS, PadField, pad_of_note};
use crate::internal::Dsp;

/// Fade-out of a gate pad after its note ended, milliseconds.
const GATE_RELEASE_MS: f32 = 40.0;

/// Level at or below which a pad is silent (the fader's bottom, −70 dB).
const SILENT_DB: f32 = -69.9;

/// One pad's parameters as plain numbers.
#[derive(Debug, Clone, Copy)]
struct Pad {
    start_ms: f32,
    end_ms: f32,
    gate: bool,
    tune: f32,
    decay_s: f32,
    drive: f32,
    pan: f32,
    level_db: f32,
    mute: bool,
    solo: bool,
    choke: u8,
}

impl Default for Pad {
    fn default() -> Pad {
        Pad {
            start_ms: 0.0,
            end_ms: MAX_MS as f32,
            gate: false,
            tune: 0.0,
            decay_s: MAX_DECAY_S as f32,
            drive: 0.0,
            pan: 0.0,
            level_db: 0.0,
            mute: false,
            solo: false,
            choke: 0,
        }
    }
}

pub(super) struct SamplerDsp {
    engine: Box<Engine>,
    sample_rate: f32,
    /// Sample slots of the engine (pad × layer); only layer 0 is used.
    samples: Vec<Option<Arc<Sample>>>,
    settings: Box<[PadSettings; NUM_PADS]>,
    layers: Box<[PadLayers; NUM_PADS]>,
    pads: [Pad; PADS],
    kit_tune: f32,
    audio: AudioSide,
    meters: Meters,
    handle: SamplerHandle,
}

impl SamplerDsp {
    /// Allocates the engine, the slots and the loader thread (UI thread).
    pub(super) fn new() -> SamplerDsp {
        let (handle, audio, meters) = load::channel();
        let sample_rate = 48_000.0;
        let mut layers = Box::new([PadLayers::default(); NUM_PADS]);
        for pad in layers.iter_mut() {
            pad.layers[0].loaded = true;
        }
        let mut dsp = SamplerDsp {
            engine: Box::new(Engine::new(sample_rate)),
            sample_rate,
            samples: vec![None; NUM_SAMPLE_SLOTS],
            settings: Box::new([PadSettings::default(); NUM_PADS]),
            layers,
            pads: [Pad::default(); PADS],
            kit_tune: 0.0,
            audio,
            meters,
            handle,
        };
        dsp.update_all();
        dsp
    }

    /// The UI side: loading, pad views, playheads.
    pub(super) fn handle(&self) -> SamplerHandle {
        self.handle.clone()
    }

    /// Puts `sample` into `pad`'s slot (the loader's hand-over, also for tests). The old sample
    /// goes back to the loader thread. Needs room in the garbage queue; returns `false` without
    /// changing anything if there is none.
    pub(super) fn set_sample(&mut self, pad: usize, sample: Option<Arc<Sample>>) -> bool {
        if self.audio.garbage.slots() == 0 {
            return false;
        }
        let slot = slot_index(pad, 0);
        self.engine.kill_slot(slot);
        let old = std::mem::replace(&mut self.samples[slot], sample);
        // Room was checked above, and only this thread pushes.
        let _ = self.audio.garbage.push(old);
        self.update_pad(pad);
        true
    }

    /// Takes what the loader has finished.
    fn take_loaded(&mut self) {
        while self.audio.garbage.slots() > 0 {
            let Ok(loaded) = self.audio.incoming.pop() else {
                break;
            };
            self.set_sample(loaded.pad, loaded.sample);
        }
    }

    fn update_all(&mut self) {
        for pad in 0..PADS {
            self.update_pad(pad);
        }
    }

    /// Recomputes the engine's settings of `pad`.
    fn update_pad(&mut self, pad: usize) {
        let p = self.pads[pad];
        let any_solo = self.pads.iter().any(|p| p.solo);
        // Start and end are milliseconds into the sample; the engine wants fractions of it.
        let (start, end) = match self.samples[slot_index(pad, 0)].as_deref() {
            Some(s) if !s.is_empty() => {
                let length_ms = 1000.0 * s.len() as f32 / s.sample_rate.max(1.0);
                let fraction = |ms: f32| (ms / length_ms).clamp(0.0, 1.0);
                let end = if f64::from(p.end_ms) >= MAX_MS {
                    1.0
                } else {
                    fraction(p.end_ms)
                };
                (fraction(p.start_ms), end)
            }
            _ => (0.0, 1.0),
        };
        let play_out = p.gate || f64::from(p.decay_s) >= MAX_DECAY_S;
        let amp = EnvParams {
            attack_ms: 0.0,
            hold_ms: 0.0,
            decay_ms: p.decay_s * 1000.0,
            sustain: if play_out { 1.0 } else { 0.0 },
            release_ms: GATE_RELEASE_MS,
        };
        let params = PadParams {
            volume_db: p.level_db,
            pan: p.pan,
            mute: p.mute || (any_solo && !p.solo) || p.level_db <= SILENT_DB,
            tune: p.tune + self.kit_tune,
            start,
            end,
            gate: p.gate,
            amp,
            drive: p.drive,
            choke_group: p.choke,
            ..PadParams::default()
        };
        self.settings[pad] = PadSettings::new(&params, self.sample_rate);
    }
}

impl Dsp for SamplerDsp {
    fn activate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        *self.engine = Engine::new(sample_rate);
        self.update_all();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        if !value.is_finite() {
            return;
        }
        match index {
            KIT_TUNE => {
                self.kit_tune = value as f32;
                self.update_all();
            }
            FOLLOW => {}
            _ => {
                let Some((pad, field)) = PadField::of_wire(index) else {
                    return;
                };
                let p = &mut self.pads[pad];
                let v = value as f32;
                match field {
                    PadField::Start => p.start_ms = v,
                    PadField::End => p.end_ms = v,
                    PadField::Mode => p.gate = value >= 0.5,
                    PadField::Tune => p.tune = v,
                    PadField::Decay => p.decay_s = v,
                    PadField::Drive => p.drive = v,
                    PadField::Pan => p.pan = v,
                    PadField::Level => p.level_db = v,
                    PadField::Mute => p.mute = value >= 0.5,
                    PadField::Solo => {
                        p.solo = value >= 0.5;
                        // Solo changes every pad's audibility.
                        self.update_all();
                        return;
                    }
                    PadField::Choke => {
                        p.choke = value.round().clamp(0.0, f64::from(CHOKE_GROUPS)) as u8
                    }
                }
                self.update_pad(pad);
            }
        }
    }

    fn process(&mut self, _input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        self.take_loaded();
        let [left, right] = output;
        let mut bus = [[0.0f32; 2]; NUM_OUTPUTS];
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            bus[0] = [0.0; 2];
            self.engine
                .render_frame(self.samples.as_slice(), &self.settings[..], &mut bus);
            *l = bus[0][0];
            *r = bus[0][1];
        }
        let heads = self.engine.playheads(self.samples.as_slice());
        for pad in 0..PADS {
            self.meters.playhead(pad, heads[slot_index(pad, 0)]);
        }
    }

    fn reset(&mut self) {
        self.engine.reset();
    }

    fn is_instrument(&self) -> bool {
        true
    }

    fn note_on(&mut self, key: u8, velocity: u8) {
        let Some(pad) = pad_of_note(key) else {
            return;
        };
        self.meters.hit(pad);
        self.engine.note_on(
            pad,
            f32::from(velocity.clamp(1, 127)) / 127.0,
            self.samples.as_slice(),
            &self.settings[..],
            &self.layers[..],
        );
    }

    fn note_off(&mut self, key: u8) {
        if let Some(pad) = pad_of_note(key) {
            self.engine.note_off(pad, &self.settings[..]);
        }
    }

    fn all_notes_off(&mut self) {
        self.engine.choke_all();
    }

    fn sampler(&self) -> Option<SamplerHandle> {
        Some(self.handle())
    }
}
