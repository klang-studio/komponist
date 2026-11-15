//! One key of knurr: up to [`MAX_UNISON`] pairs of oscillators, sub and noise, the high-pass,
//! the filter (two channels when the unison is spread), three envelopes, two LFOs, the matrix
//! and the analog drift. [`Voice::control`] runs once per control interval and turns the
//! smoothed globals ([`Control`]) into increments, coefficients and gains; [`Voice::render`] is
//! the per-sample loop.

use klang_dsp::phase::increment;
use klang_dsp::rng::Rng;
use klang_dsp::sine::sine;
use komponist_model::params::va_curve::{
    ENV_CUTOFF_OCT, MOD_CUTOFF_OCT, MOD_DETUNE_CT, MOD_PITCH_ST, MOD_PW, MOD_RATE_OCT, SUB_GAIN,
    dst, mix_gains, src,
};

use super::env::{Adsr, Coefs as EnvCoefs};
use super::filter::{Coefs as FilterCoefs, Filter, HighPass};
use super::lfo::Lfo;
use super::osc;
use crate::internal::dsp::CONTROL;

/// Most unison oscillator pairs per key.
pub const MAX_UNISON: usize = 7;

/// Gain of the noise at 100 %.
const NOISE_GAIN: f32 = 0.5;
/// The drift: ± this many semitones (6 ct) on the pitch and this many octaves on the cutoff at
/// "Analog" 100 %, and the static tolerance of each oscillator in semitones (± 3 ct) and of the
/// envelope times (± 3 %).
const DRIFT_PITCH_ST: f32 = 0.06;
const DRIFT_CUTOFF_OCT: f32 = 0.08;
const TOLERANCE_ST: f32 = 0.03;
const TOLERANCE_TIME: f32 = 0.03;
/// Seconds a drift takes to reach a new random target, as a range.
const DRIFT_SECONDS: (f32, f32) = (1.0, 3.0);

/// What an LFO needs from the globals for a control interval.
#[derive(Clone, Copy, Debug, Default)]
pub struct LfoControl {
    pub shape: usize,
    /// Hz after the tempo sync.
    pub rate_hz: f32,
    /// Restarts on a key (one state per voice) or runs free (shared).
    pub key: bool,
    pub uni: bool,
    /// Fade-in per control interval (1 = no fade).
    pub fade_step: f32,
}

/// One matrix slot.
#[derive(Clone, Copy, Debug, Default)]
pub struct Slot {
    pub src: usize,
    pub dst: usize,
    pub amount: f32,
}

/// The smoothed globals of a control interval: everything a voice needs that does not belong to
/// one key.
#[derive(Clone, Copy, Debug)]
pub struct Control {
    pub rate: f32,
    pub wave_a: usize,
    pub wave_b: usize,
    pub pw_a: f32,
    pub pw_b: f32,
    /// Oscillator B against A in semitones (octave and fine tune).
    pub b_semi: f32,
    pub mix: f32,
    pub sub: f32,
    pub noise: f32,
    pub filter_type: usize,
    /// `log2(cutoff / 1 kHz)`.
    pub cutoff_oct: f32,
    pub resonance: f32,
    pub drive: f32,
    pub key_amount: f32,
    pub hp_amount: f32,
    pub env_amount: f32,
    pub vel_filter: f32,
    pub velocity: f32,
    /// Vibrato depth of the wheel in semitones, and the wheel, pressure and bend (semitones).
    pub wheel_vib: f32,
    pub wheel: f32,
    pub pressure: f32,
    pub bend: f32,
    pub at_cut: f32,
    pub octave: f32,
    /// Detune in cents, spread, analog 0 … 1, volume as a gain.
    pub detune: f32,
    pub analog: f32,
    pub volume: f32,
    pub unison: usize,
    pub stereo: bool,
    /// Per unison pair: position −1 … 1 and the gains into left and right.
    pub pos: [f32; MAX_UNISON],
    pub pan_l: [f32; MAX_UNISON],
    pub pan_r: [f32; MAX_UNISON],
    pub slots: [Slot; 4],
    pub lfo: [LfoControl; 2],
    /// Value of the free-running LFOs (those without `Key`).
    pub lfo_free: [f32; 2],
    /// Times of the three envelopes: a, d, s, r in seconds / level.
    pub env: [[f32; 4]; 3],
    /// Changes whenever an envelope time changes: voices recompute their coefficients.
    pub env_gen: u32,
    /// The release pole of the quick fade-out ([`Voice::panic`]).
    pub panic_pole: f32,
}

/// A note to start.
#[derive(Clone, Copy, Debug)]
pub struct Note {
    pub key: u8,
    /// 0 … 1.
    pub velocity: f32,
    /// The note counter, for stealing the oldest.
    pub age: u64,
    /// Seed of the voice's random numbers (from the note counter: a project renders the same).
    pub seed: u32,
    /// The pitch the glide starts at (`None`: no glide) and its time in seconds.
    pub from: Option<f32>,
    pub glide_s: f32,
}

/// A slow random walk: a new target every one to three seconds, reached in a straight line.
#[derive(Clone, Copy, Debug, Default)]
struct Drift {
    value: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Drift {
    fn step(&mut self, rng: &mut Rng, rate: f32) -> f32 {
        if self.left == 0 {
            self.target = rng.bipolar();
            let seconds = DRIFT_SECONDS.0 + (DRIFT_SECONDS.1 - DRIFT_SECONDS.0) * rng.unipolar();
            self.left = ((seconds * rate / CONTROL as f32) as u32).max(1);
            self.step = (self.target - self.value) / self.left as f32;
        }
        self.left -= 1;
        self.value += self.step;
        self.value
    }
}

/// Whether a voice sounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Free.
    Off,
    /// Its key is down.
    Held,
    /// Its key is up, the amplitude envelope runs out.
    Released,
}

/// One key.
pub struct Voice {
    pub state: State,
    pub key: u8,
    pub velocity: f32,
    /// The note counter at the start, for stealing the oldest.
    pub age: u64,
    /// Fades out quickly instead of running the release (all notes off).
    pub panic: bool,
    /// Current pitch as a (fractional) MIDI note, and the glide to its target.
    pub pitch: f32,
    glide_target: f32,
    glide_step: f32,
    glide_left: u32,
    rng: Rng,
    random: f32,
    drift: [Drift; 2],
    tolerance: [f32; 2],
    time_scale: [f32; 3],
    phase_a: [u32; MAX_UNISON],
    phase_b: [u32; MAX_UNISON],
    phase_sub: u32,
    inc_a: [u32; MAX_UNISON],
    inc_b: [u32; MAX_UNISON],
    inc_sub: u32,
    pw: [f32; 2],
    gains: [f32; 2],
    env: [Adsr; 3],
    env_coefs: [EnvCoefs; 3],
    env_gen: u32,
    lfo: [Lfo; 2],
    lfo_out: [f32; 2],
    /// The rate modulation of the last interval (the LFOs run before the matrix).
    rate_mod: [f32; 2],
    hp: [HighPass; 2],
    filter: [Filter; 2],
    filter_coefs: FilterCoefs,
    gain: f32,
    gain_step: f32,
}

impl Voice {
    /// A free voice.
    pub fn new() -> Voice {
        Voice {
            state: State::Off,
            key: 0,
            velocity: 0.0,
            age: 0,
            panic: false,
            pitch: 60.0,
            glide_target: 60.0,
            glide_step: 0.0,
            glide_left: 0,
            rng: Rng::default(),
            random: 0.0,
            drift: [Drift::default(); 2],
            tolerance: [0.0; 2],
            time_scale: [1.0; 3],
            phase_a: [0; MAX_UNISON],
            phase_b: [0; MAX_UNISON],
            phase_sub: 0,
            inc_a: [0; MAX_UNISON],
            inc_b: [0; MAX_UNISON],
            inc_sub: 0,
            pw: [0.5; 2],
            gains: [1.0; 2],
            env: [Adsr::default(); 3],
            env_coefs: [EnvCoefs::new(0.01, 0.1, 0.5, 0.1, 48_000.0, [1.0; 3]); 3],
            env_gen: u32::MAX,
            lfo: [Lfo::new(1); 2],
            lfo_out: [0.0; 2],
            rate_mod: [0.0; 2],
            hp: [HighPass::default(); 2],
            filter: [Filter::default(); 2],
            filter_coefs: FilterCoefs::default(),
            gain: 0.0,
            gain_step: 0.0,
        }
    }

    /// Free again, silent, without a trace of the last note.
    pub fn reset(&mut self) {
        *self = Voice::new();
    }

    /// The level of the amplitude envelope.
    pub fn level(&self) -> f32 {
        self.env[0].level
    }

    /// Starts a note. A voice that still sounds (stolen) keeps its phases, filter state and
    /// amplitude level, so the takeover does not click; the envelopes attack from where they are.
    pub fn start(&mut self, note: &Note, c: &Control) {
        let fresh = self.state == State::Off;
        let Note { key, .. } = *note;
        self.rng = Rng::new(note.seed);
        self.key = key;
        self.velocity = note.velocity;
        self.age = note.age;
        self.panic = false;
        self.state = State::Held;
        self.random = self.rng.bipolar();
        self.tolerance = [self.rng.bipolar(), self.rng.bipolar()];
        self.time_scale = [
            1.0 + TOLERANCE_TIME * c.analog * self.rng.bipolar(),
            1.0 + TOLERANCE_TIME * c.analog * self.rng.bipolar(),
            1.0 + TOLERANCE_TIME * c.analog * self.rng.bipolar(),
        ];
        self.env_gen = u32::MAX;
        self.set_pitch(key, note.from, note.glide_s, c.rate);
        if fresh {
            self.filter = [Filter::default(); 2];
            self.hp = [HighPass::default(); 2];
            self.gain = 0.0;
            self.gain_step = 0.0;
            self.drift = [Drift::default(); 2];
            for u in 0..MAX_UNISON {
                // One unison pair on the same phase would add up to a louder, harsher start.
                let random = c.unison > 1;
                self.phase_a[u] = if random { self.rng.next_u32() } else { 0 };
                self.phase_b[u] = if random { self.rng.next_u32() } else { 0 };
            }
            self.phase_sub = 0;
        }
        for (i, lfo) in self.lfo.iter_mut().enumerate() {
            *lfo = Lfo::new(self.rng.next_u32() | 1);
            if c.lfo[i].key {
                lfo.restart();
            }
        }
        self.lfo_out = [0.0; 2];
        self.rate_mod = [0.0; 2];
        for env in &mut self.env {
            env.gate_on();
        }
    }

    /// Moves to `key` (mono/legato transitions and note starts): glides from `from` over
    /// `glide_s` seconds, or jumps.
    pub fn set_pitch(&mut self, key: u8, from: Option<f32>, glide_s: f32, rate: f32) {
        self.glide_target = f32::from(key);
        match from {
            Some(start) if glide_s > 0.0 => {
                self.pitch = start;
                self.glide_left = ((glide_s * rate / CONTROL as f32) as u32).max(1);
                self.glide_step = (self.glide_target - start) / self.glide_left as f32;
            }
            _ => {
                self.pitch = self.glide_target;
                self.glide_left = 0;
                self.glide_step = 0.0;
            }
        }
    }

    /// Restarts the envelopes (a new note in mono mode).
    pub fn retrigger(&mut self) {
        for env in &mut self.env {
            env.gate_on();
        }
    }

    /// Key up.
    pub fn release(&mut self) {
        if self.state == State::Held {
            self.state = State::Released;
            for env in &mut self.env {
                env.gate_off();
            }
        }
    }

    fn recompute_envelopes(&mut self, c: &Control) {
        for i in 0..3 {
            let [a, d, s, r] = c.env[i];
            self.env_coefs[i] = EnvCoefs::new(a, d, s, r, c.rate, [self.time_scale[i]; 3]);
        }
        self.env_gen = c.env_gen;
    }

    /// One control interval (every [`CONTROL`] samples) of a sounding voice.
    pub fn control(&mut self, c: &Control) {
        if self.env_gen != c.env_gen {
            self.recompute_envelopes(c);
        }
        let quick = self.panic.then_some(c.panic_pole);
        let amp = self.env[0].step(&self.env_coefs[0], quick);
        let filt = self.env[1].step(&self.env_coefs[1], None);
        let env3 = self.env[2].step(&self.env_coefs[2], None);
        if self.state == State::Released && self.env[0].is_idle() {
            self.state = State::Off;
            self.gain = 0.0;
            self.gain_step = 0.0;
            return;
        }

        // LFOs: a voice's own, or the shared value.
        for l in 0..2 {
            let lc = &c.lfo[l];
            self.lfo_out[l] = if lc.key {
                let rate = lc.rate_hz * (self.rate_mod[l] * MOD_RATE_OCT as f32).exp2();
                let inc = Lfo::increment(rate.clamp(0.001, 200.0), c.rate);
                self.lfo[l].step(lc.shape, inc, lc.fade_step, lc.uni)
            } else {
                c.lfo_free[l]
            };
        }

        // The matrix.
        let sources = [
            0.0,
            self.lfo_out[0],
            self.lfo_out[1],
            env3,
            self.velocity,
            (f32::from(self.key) - 60.0) / 60.0,
            c.wheel,
            c.pressure,
            self.random,
        ];
        let mut m = [0.0f32; dst::COUNT];
        for slot in &c.slots {
            if slot.src != src::OFF && slot.dst != dst::OFF {
                m[slot.dst] += slot.amount * sources[slot.src];
            }
        }
        self.rate_mod = [m[dst::LFO1_RATE], m[dst::LFO2_RATE]];

        // Pitch.
        if self.glide_left > 0 {
            self.glide_left -= 1;
            self.pitch = if self.glide_left == 0 {
                self.glide_target
            } else {
                self.pitch + self.glide_step
            };
        }
        let drift_pitch = self.drift[0].step(&mut self.rng, c.rate);
        let drift_cutoff = self.drift[1].step(&mut self.rng, c.rate);
        let vibrato = c.wheel_vib * c.wheel * self.lfo_out[0];
        let semi_a = self.pitch
            + c.octave
            + c.bend
            + m[dst::PITCH] * MOD_PITCH_ST as f32
            + vibrato
            + c.analog * (DRIFT_PITCH_ST * drift_pitch + TOLERANCE_ST * self.tolerance[0]);
        let semi_b = semi_a
            + c.b_semi
            + m[dst::PITCH_B] * MOD_PITCH_ST as f32
            + c.analog * TOLERANCE_ST * (self.tolerance[1] - self.tolerance[0]);
        let limit = 0.45 * c.rate;
        let hz = |semi: f32| (440.0 * ((semi - 69.0) * (1.0 / 12.0)).exp2()).clamp(1.0, limit);
        let base_a = increment(hz(semi_a), c.rate);
        let base_b = increment(hz(semi_b), c.rate);
        let detune = (c.detune + m[dst::DETUNE] * MOD_DETUNE_CT as f32).clamp(0.0, 200.0);
        for u in 0..c.unison {
            let ratio = (c.pos[u] * detune * (1.0 / 1200.0)).exp2();
            self.inc_a[u] = (base_a as f32 * ratio) as u32;
            self.inc_b[u] = (base_b as f32 * ratio) as u32;
        }
        self.inc_sub = base_a >> 1;

        // Oscillator shape and balance.
        self.pw = [
            (c.pw_a + m[dst::PW_A] * MOD_PW as f32).clamp(0.02, 0.98),
            (c.pw_b + m[dst::PW_B] * MOD_PW as f32).clamp(0.02, 0.98),
        ];
        let (ga, gb) = mix_gains(f64::from((c.mix + m[dst::MIX]).clamp(0.0, 1.0)));
        self.gains = [ga as f32, gb as f32];

        // Filter.
        let env_amount = c.env_amount * (1.0 - c.vel_filter + c.vel_filter * self.velocity);
        let oct = c.cutoff_oct
            + env_amount * filt * ENV_CUTOFF_OCT as f32
            + c.key_amount * (f32::from(self.key) - 60.0) * (1.0 / 12.0)
            + m[dst::CUTOFF] * MOD_CUTOFF_OCT as f32
            + c.at_cut * c.pressure * MOD_CUTOFF_OCT as f32
            + c.analog * DRIFT_CUTOFF_OCT * drift_cutoff;
        let cutoff = (1000.0 * oct.clamp(-8.0, 8.0).exp2()).clamp(20.0, limit);
        self.filter_coefs = FilterCoefs::new(
            c.filter_type,
            cutoff,
            (c.resonance + m[dst::RESONANCE]).clamp(0.0, 1.0),
            (c.drive + m[dst::DRIVE]).clamp(0.0, 1.0),
            c.rate,
        );
        for ch in 0..2 {
            self.filter[ch].tidy();
            self.hp[ch].tidy();
        }

        // Level: envelope, velocity, volume, the matrix; a ramp from the last interval's value.
        let vel = 1.0 - c.velocity + c.velocity * self.velocity;
        let target = amp * vel * c.volume * (1.0 + m[dst::VOLUME]).max(0.0);
        // `render` has advanced `gain` to the end of the last interval.
        self.gain_step = (target - self.gain) / CONTROL as f32;
    }

    /// Adds `n` samples of this voice to `out`, `done` samples into the control interval.
    pub fn render(&mut self, c: &Control, out: [&mut [f32]; 2], n: usize) {
        let [out_l, out_r] = out;
        let uni = c.unison;
        let (wave_a, wave_b) = (c.wave_a, c.wave_b);
        let [ga, gb] = self.gains;
        let sub = c.sub * SUB_GAIN as f32;
        let noise = c.noise * NOISE_GAIN;
        let hp = c.hp_amount;
        let mut gain = self.gain;
        for i in 0..n {
            let mut l = 0.0f32;
            let mut r = 0.0f32;
            for u in 0..uni {
                let a = osc::sample(wave_a, self.phase_a[u], self.inc_a[u], self.pw[0]);
                let b = osc::sample(wave_b, self.phase_b[u], self.inc_b[u], self.pw[1]);
                self.phase_a[u] = self.phase_a[u].wrapping_add(self.inc_a[u]);
                self.phase_b[u] = self.phase_b[u].wrapping_add(self.inc_b[u]);
                let s = ga * a + gb * b;
                l += s * c.pan_l[u];
                r += s * c.pan_r[u];
            }
            let extra = sub * sine(self.phase_sub) + noise * self.rng.bipolar();
            self.phase_sub = self.phase_sub.wrapping_add(self.inc_sub);
            l += extra;
            r += extra;
            let yl = {
                let x = self.hp[0].tick(l, hp);
                self.filter[0].tick(x, &self.filter_coefs)
            };
            let yr = if c.stereo {
                let x = self.hp[1].tick(r, hp);
                self.filter[1].tick(x, &self.filter_coefs)
            } else {
                yl
            };
            gain += self.gain_step;
            out_l[i] += yl * gain;
            out_r[i] += yr * gain;
        }
        self.gain = gain;
    }

    /// Whether the voice has to be rendered.
    pub fn sounds(&self) -> bool {
        self.state != State::Off
    }

    /// Checks the audio state for NaN or infinity and silences the voice if it is damaged.
    pub fn sanitize(&mut self) {
        if !self.gain.is_finite() || !self.gain_step.is_finite() {
            self.reset();
        }
    }
}
