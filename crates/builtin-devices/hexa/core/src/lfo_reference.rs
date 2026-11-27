//! The LFO runtime as it was before #352 (`Lfos` of `lfo.rs` at commit `e97f261c`), copied
//! verbatim as a differential oracle: `lfo_baseline` drives it and the production `Lfos` with the
//! same inputs and compares their outputs bit for bit. Only the imports differ (the parameter
//! types and constants stay in `lfo.rs`). Never change the logic below; it pins the behaviour the
//! LFO adoption (#360) must keep.

use klang_dsp::rng::Rng;
use klang_dsp::wave;

use crate::MAX_VOICES;
use crate::lfo::{LfoParams, LfoWaveform, MAX_FADE_IN, MAX_LFOS};
use crate::modulation::{LfoModulation, ModTarget, Offsets};

/// Start of the sample-and-hold sequence, also in place of seed 0.
const SEED: u32 = 0x2545_f491;

/// Runtime state of all LFOs across all voices.
pub(crate) struct Lfos {
    /// Free-running phase per LFO, joined by voices without key sync.
    global: [u32; MAX_LFOS],
    phase: [[u32; MAX_VOICES]; MAX_LFOS],
    /// Current sample-and-hold value per LFO and voice.
    held: [[f32; MAX_VOICES]; MAX_LFOS],
    /// Output at the start of the next block per LFO and voice.
    value: [[f32; MAX_VOICES]; MAX_LFOS],
    /// The level (amount × fade) behind `value`, for displays.
    level: [[f32; MAX_VOICES]; MAX_LFOS],
    /// Seconds since note-on per voice, for the fade-in.
    age: [f32; MAX_VOICES],
    /// Random values for sample and hold.
    rng: Rng,
}

impl Lfos {
    pub(crate) fn new() -> Self {
        Self {
            global: [0; MAX_LFOS],
            phase: [[0; MAX_VOICES]; MAX_LFOS],
            held: [[0.0; MAX_VOICES]; MAX_LFOS],
            value: [[0.0; MAX_VOICES]; MAX_LFOS],
            level: [[0.0; MAX_VOICES]; MAX_LFOS],
            age: [0.0; MAX_VOICES],
            rng: Rng::new(SEED),
        }
    }

    /// Restarts the sample-and-hold random sequence from `seed` (0 is
    /// replaced, xorshift would stay at 0).
    pub(crate) fn seed(&mut self, seed: u32) {
        self.rng = Rng::new(if seed == 0 { SEED } else { seed });
    }

    /// Starts the LFOs of `voice` for a new note. All are started, so an LFO
    /// that gets used while the note sounds has a sensible phase.
    pub(crate) fn note_on(&mut self, voice: usize, params: &[LfoParams]) {
        self.age[voice] = 0.0;
        for (l, p) in params.iter().enumerate() {
            self.start(l, voice, p);
        }
    }

    /// Starts LFO `l` of `voice`: at note-on, or when the LFO is added while
    /// the voice sounds (then its fade-in counts from the note-on).
    pub(crate) fn start(&mut self, l: usize, voice: usize, p: &LfoParams) {
        let offset = crate::phase::from_unit(p.phase);
        self.phase[l][voice] = if p.key_sync {
            offset
        } else {
            self.global[l].wrapping_add(offset)
        };
        self.held[l][voice] = self.random();
        self.set_output(l, voice, p, p.amount, p.fade_in);
    }

    /// Phase, level and output of LFO `l` of `voice`, see [`LfoPositions`].
    ///
    /// [`LfoPositions`]: crate::lfo::LfoPositions
    pub(crate) fn position(&self, l: usize, voice: usize) -> [f32; 3] {
        let phase = crate::phase::to_unit(self.phase[l][voice]);
        [phase, self.level[l][voice], self.value[l][voice]]
    }

    /// Advances the free-running phases by one block; call once per block.
    pub(crate) fn advance_global(&mut self, params: &[LfoParams], block_seconds: f32) {
        for (l, p) in params.iter().enumerate() {
            self.global[l] = self.global[l].wrapping_add(increment(p.rate, block_seconds));
        }
    }

    /// Advances the LFOs in `used` (bit mask) of a sounding `voice` by one
    /// block, with their own modulation (rate, amount and fade-in offsets
    /// from every source but LFOs, where `modulated` says) at the start and
    /// the end of the block.
    /// Returns each LFO's output at the start and at the end of the block; 0
    /// for unused ones.
    pub(crate) fn advance_voice(
        &mut self,
        voice: usize,
        params: &[LfoParams],
        used: u8,
        modulated: LfoModulation,
        (o0, o1): (&Offsets, &Offsets),
        block_seconds: f32,
    ) -> ([f32; MAX_LFOS], [f32; MAX_LFOS]) {
        self.age[voice] += block_seconds;
        let mut start = [0.0; MAX_LFOS];
        let mut end = [0.0; MAX_LFOS];
        for (l, p) in params.iter().enumerate() {
            if used & (1 << l) == 0 {
                continue;
            }
            start[l] = self.value[l][voice];
            let old = self.phase[l][voice];
            let bit = 1 << l;
            let octaves = if modulated.rates & bit != 0 {
                o0.get(ModTarget::LfoRate(l))
            } else {
                0.0
            };
            let rate = if octaves != 0.0 {
                p.rate * libm::exp2f(octaves)
            } else {
                p.rate
            };
            let new = old.wrapping_add(increment(rate, block_seconds));
            self.phase[l][voice] = new;
            if new < old {
                self.held[l][voice] = self.random();
            }
            let amount = if modulated.amounts & bit != 0 {
                (p.amount + o1.get(ModTarget::LfoAmount(l))).clamp(0.0, 1.0)
            } else {
                p.amount
            };
            let fade_in = if modulated.fades & bit != 0 {
                crate::modulation::time_offset(
                    p.fade_in,
                    o1.get(ModTarget::LfoFade(l)),
                    MAX_FADE_IN,
                )
            } else {
                p.fade_in
            };
            self.set_output(l, voice, p, amount, fade_in);
            end[l] = self.value[l][voice];
        }
        (start, end)
    }

    fn set_output(&mut self, l: usize, voice: usize, p: &LfoParams, amount: f32, fade_in: f32) {
        let age = self.age[voice] - p.delay;
        let fade = if age < 0.0 {
            0.0
        } else if fade_in > 0.0 {
            (age / fade_in).min(1.0)
        } else {
            1.0
        };
        let level = fade * amount;
        self.level[l][voice] = level;
        self.value[l][voice] = shape(p.waveform, self.phase[l][voice], self.held[l][voice]) * level;
    }

    /// Uniform in -1..1.
    fn random(&mut self) -> f32 {
        self.rng.bipolar()
    }
}

/// Phase increment for `rate` Hz over `seconds`, one period = 2^32.
fn increment(rate: f32, seconds: f32) -> u32 {
    (rate as f64 * seconds as f64 * crate::phase::PHASE_PERIOD) as u64 as u32
}

/// Value of `waveform` at `phase`; `held` is the sample-and-hold value.
fn shape(waveform: LfoWaveform, phase: u32, held: f32) -> f32 {
    match waveform {
        LfoWaveform::Sine => wave::sine(phase),
        LfoWaveform::Triangle => wave::triangle(phase),
        LfoWaveform::SawUp => wave::saw_up(phase),
        LfoWaveform::SawDown => wave::saw_down(phase),
        LfoWaveform::Square => wave::square(phase),
        LfoWaveform::SampleHold => held,
    }
}
