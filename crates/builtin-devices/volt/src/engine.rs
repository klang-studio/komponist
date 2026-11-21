//! The engine of volt: the voice pool, the note logic (poly, mono, legato, glide, stealing), the
//! smoothing and the control-rate step. See the module docs of [`super`] for the signal flow.

use klang_dsp::rng::Rng;
use komponist_model::params::va_curve::{LFO_DIV_BEATS, dst};

use super::env::pole;
use super::lfo::Lfo;
use super::params::{COUNT, MODES, OFF_S, id, lfo, lfo_base, mod_src};
use super::voice::{Control, LfoControl, MAX_UNISON, Note, Slot, State, Voice};
use komponist_device_core::Dsp;
use komponist_device_core::dsp::{CONTROL, db_to_gain, finite};

/// Voices in the pool.
pub const VOICES: usize = 16;
/// The most unison pairs that sound at once over all keys: `voices × unison` is capped here, so
/// a seven-voice unison leaves six keys, five leave nine, three or fewer all sixteen.
pub const MAX_GROUPS: usize = 48;
/// Entries of the mono note stack.
const STACK: usize = 8;
/// The seed all randomness starts from; with the note counter it makes a project render the same
/// every time.
const SEED: u32 = 0x6B6E_7572;
/// Seconds the parameters and wheels take to follow (63 %).
const SMOOTH_S: f32 = 0.010;
/// Seconds a note takes to fade out on "all notes off".
const PANIC_S: f32 = 0.005;

/// The parameters that move smoothly.
#[derive(Clone, Copy, Debug, Default)]
struct Smoothed {
    cutoff_oct: f32,
    resonance: f32,
    drive: f32,
    mix: f32,
    volume: f32,
    pw_a: f32,
    pw_b: f32,
    b_fine: f32,
    sub: f32,
    noise: f32,
    hp_hz: f32,
    detune: f32,
    wheel: f32,
    pressure: f32,
    bend: f32,
}

#[inline]
fn chase(value: &mut f32, target: f32, amount: f32) {
    klang_dsp::one_pole::chase(value, target, amount);
}

/// The synthesizer.
pub struct Volt {
    rate: f32,
    values: [f64; COUNT],
    voices: Vec<Voice>,
    ctl: Control,
    sm: Smoothed,
    /// The smoothed values jump onto their targets (first step, after a reset).
    snap: bool,
    smooth_amount: f32,
    /// Samples into the current control interval; runs across calls.
    ctrl_pos: usize,
    tempo: f64,
    free: [Lfo; 2],
    notes: u64,
    last_pitch: f32,
    stack: [(u8, f32); STACK],
    stack_len: usize,
    /// Wheels as the MIDI input last set them (smoothed into `sm`).
    wheel: f32,
    pressure: f32,
    bend: f32,
    env_gen: u32,
}

impl Default for Volt {
    fn default() -> Self {
        Volt::new()
    }
}

impl Volt {
    /// A synthesizer with the parameters at their defaults.
    pub fn new() -> Volt {
        let mut values = [0.0; COUNT];
        for (v, info) in values
            .iter_mut()
            .zip(super::params::params(komponist_i18n::Lang::De))
        {
            *v = info.default;
        }
        let mut k = Volt {
            rate: 48_000.0,
            values,
            voices: (0..VOICES).map(|_| Voice::new()).collect(),
            ctl: blank_control(),
            sm: Smoothed::default(),
            snap: true,
            smooth_amount: 0.0,
            ctrl_pos: 0,
            tempo: 120.0,
            free: [Lfo::new(SEED), Lfo::new(SEED ^ 0x5555)],
            notes: 0,
            last_pitch: 60.0,
            stack: [(0, 0.0); STACK],
            stack_len: 0,
            wheel: 0.0,
            pressure: 0.0,
            bend: 0.0,
            env_gen: 0,
        };
        k.set_rate(48_000.0);
        k
    }

    fn set_rate(&mut self, rate: f32) {
        self.rate = rate;
        self.smooth_amount = 1.0 - (-(CONTROL as f32) / (SMOOTH_S * rate)).exp();
        self.snap = true;
        self.rebuild();
    }

    fn value(&self, index: usize) -> f32 {
        self.values[index] as f32
    }

    /// Voices that can sound at once with the current unison.
    pub fn max_poly(&self) -> usize {
        let unison = self.values[id::UNISON].round().max(1.0) as usize;
        (MAX_GROUPS / unison).clamp(
            1,
            (self.values[id::VOICES].round() as usize).clamp(1, VOICES),
        )
    }

    /// Moves the smoothed values one control interval towards their targets.
    fn smooth_step(&mut self) {
        let t = |k: usize| self.values[k] as f32;
        let targets = Smoothed {
            cutoff_oct: (t(id::CUTOFF) / 1000.0).log2(),
            resonance: t(id::RESONANCE),
            drive: t(id::DRIVE),
            mix: t(id::OSC_MIX),
            volume: db_to_gain(t(id::VOLUME)),
            pw_a: t(id::OSC_A_PW),
            pw_b: t(id::OSC_B_PW),
            b_fine: t(id::OSC_B_FINE),
            sub: t(id::SUB_LEVEL),
            noise: t(id::NOISE_LEVEL),
            hp_hz: t(id::HP_FREQ),
            detune: t(id::DETUNE),
            wheel: self.wheel,
            pressure: self.pressure,
            bend: self.bend,
        };
        let a = if self.snap { 1.0 } else { self.smooth_amount };
        self.snap = false;
        let s = &mut self.sm;
        chase(&mut s.cutoff_oct, targets.cutoff_oct, a);
        chase(&mut s.resonance, targets.resonance, a);
        chase(&mut s.drive, targets.drive, a);
        chase(&mut s.mix, targets.mix, a);
        chase(&mut s.volume, targets.volume, a);
        chase(&mut s.pw_a, targets.pw_a, a);
        chase(&mut s.pw_b, targets.pw_b, a);
        chase(&mut s.b_fine, targets.b_fine, a);
        chase(&mut s.sub, targets.sub, a);
        chase(&mut s.noise, targets.noise, a);
        chase(&mut s.hp_hz, targets.hp_hz, a);
        chase(&mut s.detune, targets.detune, a);
        chase(&mut s.wheel, targets.wheel, a);
        chase(&mut s.pressure, targets.pressure, a);
        chase(&mut s.bend, targets.bend, a);
    }

    /// Builds the control block from the parameters and the smoothed values.
    fn rebuild(&mut self) {
        if self.snap {
            // Before the first step the smoothed values are unset: take the targets.
            self.smooth_step();
        }
        let v = |k: usize| self.values[k] as f32;
        let unison = (v(id::UNISON).round().max(1.0) as usize).min(MAX_UNISON);
        let spread = v(id::SPREAD);
        let stereo = unison > 1 && spread > 0.001;
        let norm = if unison > 1 {
            1.0 / (unison as f32).sqrt()
        } else {
            1.0
        };
        let mut pos = [0.0; MAX_UNISON];
        let mut pan_l = [norm; MAX_UNISON];
        let mut pan_r = [norm; MAX_UNISON];
        for u in 0..unison {
            if unison > 1 {
                pos[u] = 2.0 * u as f32 / (unison - 1) as f32 - 1.0;
            }
            if stereo {
                let angle = (pos[u] * spread + 1.0) * core::f32::consts::FRAC_PI_4;
                pan_l[u] = norm * core::f32::consts::SQRT_2 * angle.cos();
                pan_r[u] = norm * core::f32::consts::SQRT_2 * angle.sin();
            }
        }
        let mut slots = [Slot::default(); 4];
        for (i, slot) in slots.iter_mut().enumerate() {
            let base = mod_src(i);
            *slot = Slot {
                src: self.values[base].round() as usize,
                dst: (self.values[base + 1].round() as usize).min(dst::COUNT - 1),
                amount: self.value(base + 2),
            };
        }
        let mut lfos = [LfoControl::default(); 2];
        for (n, l) in lfos.iter_mut().enumerate() {
            let base = lfo_base(n);
            let sync = self.values[base + lfo::SYNC] >= 0.5;
            let beats = LFO_DIV_BEATS[(self.values[base + lfo::DIV].round() as usize).min(7)];
            let fade = self.value(base + lfo::FADE);
            *l = LfoControl {
                shape: self.values[base + lfo::SHAPE].round() as usize,
                rate_hz: if sync {
                    (self.tempo / 60.0 / beats) as f32
                } else {
                    self.value(base + lfo::RATE)
                },
                key: self.values[base + lfo::KEY] >= 0.5,
                uni: self.values[base + lfo::UNI] >= 0.5,
                fade_step: if fade <= OFF_S as f32 * 1.05 {
                    1.0
                } else {
                    CONTROL as f32 / (fade * self.rate)
                },
            };
        }
        let env = |a: usize| {
            [
                self.value(a),
                self.value(a + 1),
                self.value(a + 2),
                self.value(a + 3),
            ]
        };
        let s = &self.sm;
        self.ctl = Control {
            rate: self.rate,
            wave_a: self.values[id::OSC_A_WAVE].round() as usize,
            wave_b: self.values[id::OSC_B_WAVE].round() as usize,
            pw_a: s.pw_a,
            pw_b: s.pw_b,
            b_semi: v(id::OSC_B_OCT) * 12.0 + s.b_fine * 0.01,
            mix: s.mix,
            sub: s.sub,
            noise: s.noise,
            filter_type: self.values[id::FILTER_TYPE].round() as usize,
            cutoff_oct: s.cutoff_oct,
            resonance: s.resonance,
            drive: s.drive,
            key_amount: v(id::KEY_AMOUNT),
            hp_amount: 1.0
                - (-core::f32::consts::TAU * s.hp_hz.min(0.45 * self.rate) / self.rate).exp(),
            env_amount: v(id::ENV_AMOUNT),
            vel_filter: v(id::VEL_FILTER),
            velocity: v(id::VELOCITY),
            wheel_vib: v(id::WHEEL_VIB) * 0.01,
            wheel: s.wheel,
            pressure: s.pressure,
            bend: s.bend * v(id::BEND),
            at_cut: v(id::AT_CUT),
            octave: v(id::OCTAVE) * 12.0,
            detune: s.detune,
            analog: v(id::ANALOG),
            volume: s.volume,
            unison,
            stereo,
            pos,
            pan_l,
            pan_r,
            slots,
            lfo: lfos,
            lfo_free: self.ctl.lfo_free,
            env: [env(id::AMP_A), env(id::FILT_A), env(id::MOD_A)],
            env_gen: self.env_gen,
            panic_pole: pole(PANIC_S, self.rate),
        };
    }

    /// One control interval: smoothing, shared LFOs, every sounding voice.
    fn control_step(&mut self) {
        self.smooth_step();
        self.rebuild();
        for n in 0..2 {
            let l = self.ctl.lfo[n];
            if !l.key {
                let inc = Lfo::increment(l.rate_hz, self.rate);
                self.ctl.lfo_free[n] = self.free[n].step(l.shape, inc, 1.0, l.uni);
            }
        }
        for voice in self.voices.iter_mut().filter(|v| v.sounds()) {
            voice.control(&self.ctl);
            voice.sanitize();
        }
    }

    fn glide_seconds(&self) -> f32 {
        let g = self.value(id::GLIDE);
        if g <= OFF_S as f32 * 1.05 { 0.0 } else { g }
    }

    /// The glide for a note that starts at `from`: none unless the glide is on, and with
    /// "Glide Legato" only while another key is held.
    fn glide_for(&self, from: f32, another_held: bool) -> (Option<f32>, f32) {
        let g = self.glide_seconds();
        if g <= 0.0 || (self.values[id::GLIDE_LEGATO] >= 0.5 && !another_held) {
            (None, 0.0)
        } else {
            (Some(from), g)
        }
    }

    fn note(&mut self, key: u8, velocity: f32, from: Option<f32>, glide_s: f32) -> Note {
        self.notes += 1;
        let mut seed_rng = Rng::new(SEED ^ (self.notes as u32).wrapping_mul(0x9E37_79B9));
        Note {
            key,
            velocity,
            age: self.notes,
            seed: seed_rng.next_u32() | 1,
            from,
            glide_s,
        }
    }

    /// A voice for a new note: a free one, or the quietest released, or the oldest held.
    fn allocate(&self) -> usize {
        let active = self.voices.iter().filter(|v| v.sounds()).count();
        if let Some(free) = self
            .voices
            .iter()
            .position(|v| !v.sounds())
            .filter(|_| active < self.max_poly())
        {
            return free;
        }
        let released = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.state == State::Released)
            .min_by(|a, b| a.1.level().total_cmp(&b.1.level()));
        if let Some((i, _)) = released {
            return i;
        }
        self.voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.sounds())
            .min_by_key(|(_, v)| v.age)
            .map_or(0, |(i, _)| i)
    }

    fn poly_on(&mut self, key: u8, velocity: f32) {
        let another_held = self.voices.iter().any(|v| v.state == State::Held);
        let (from, glide_s) = self.glide_for(self.last_pitch, another_held);
        let note = self.note(key, velocity, from, glide_s);
        let index = self
            .voices
            .iter()
            .position(|v| v.state == State::Held && v.key == key)
            .unwrap_or_else(|| self.allocate());
        let ctl = self.ctl;
        self.voices[index].start(&note, &ctl);
    }

    fn mono_on(&mut self, key: u8, velocity: f32, legato: bool) {
        self.stack_remove(key);
        if self.stack_len == STACK {
            self.stack.copy_within(1.., 0);
            self.stack_len -= 1;
        }
        self.stack[self.stack_len] = (key, velocity);
        self.stack_len += 1;
        let held = self.voices[0].state == State::Held;
        let from = if self.voices[0].sounds() {
            self.voices[0].pitch
        } else {
            self.last_pitch
        };
        let (glide_from, glide_s) = self.glide_for(from, held);
        if held {
            let rate = self.rate;
            let v = &mut self.voices[0];
            v.set_pitch(key, glide_from, glide_s, rate);
            v.key = key;
            v.velocity = velocity;
            if !legato {
                v.retrigger();
            }
        } else {
            let note = self.note(key, velocity, glide_from, glide_s);
            let ctl = self.ctl;
            self.voices[0].start(&note, &ctl);
        }
    }

    fn mono_off(&mut self, key: u8, legato: bool) {
        self.stack_remove(key);
        if self.voices[0].state != State::Held || self.voices[0].key != key {
            return;
        }
        if self.stack_len == 0 {
            self.voices[0].release();
            return;
        }
        let (back, velocity) = self.stack[self.stack_len - 1];
        let g = self.glide_seconds();
        let rate = self.rate;
        let v = &mut self.voices[0];
        let glide = (g > 0.0).then_some(v.pitch);
        v.set_pitch(back, glide, g, rate);
        v.key = back;
        v.velocity = velocity;
        if !legato {
            v.retrigger();
        }
    }

    fn stack_remove(&mut self, key: u8) {
        if let Some(i) = self.stack[..self.stack_len].iter().position(|e| e.0 == key) {
            self.stack.copy_within(i + 1..self.stack_len, i);
            self.stack_len -= 1;
        }
    }

    /// The keys of the sounding voices, in pool order (tests).
    #[cfg(test)]
    pub fn sounding_keys(&self) -> Vec<u8> {
        self.voices
            .iter()
            .filter(|v| v.sounds())
            .map(|v| v.key)
            .collect()
    }

    /// Hz an LFO runs at now (tests).
    #[cfg(test)]
    pub fn lfo_rate_hz(&mut self, lfo: usize) -> f32 {
        self.rebuild();
        self.ctl.lfo[lfo].rate_hz
    }

    fn mode(&self) -> usize {
        (self.values[id::MODE].round() as usize).min(MODES.len() - 1)
    }
}

fn blank_control() -> Control {
    Control {
        rate: 48_000.0,
        wave_a: 2,
        wave_b: 3,
        pw_a: 0.5,
        pw_b: 0.5,
        b_semi: 0.0,
        mix: 0.5,
        sub: 0.0,
        noise: 0.0,
        filter_type: 0,
        cutoff_oct: 0.0,
        resonance: 0.0,
        drive: 0.0,
        key_amount: 0.0,
        hp_amount: 0.0,
        env_amount: 0.0,
        vel_filter: 0.0,
        velocity: 0.0,
        wheel_vib: 0.0,
        wheel: 0.0,
        pressure: 0.0,
        bend: 0.0,
        at_cut: 0.0,
        octave: 0.0,
        detune: 0.0,
        analog: 0.0,
        volume: 1.0,
        unison: 1,
        stereo: false,
        pos: [0.0; MAX_UNISON],
        pan_l: [1.0; MAX_UNISON],
        pan_r: [1.0; MAX_UNISON],
        slots: [Slot::default(); 4],
        lfo: [LfoControl::default(); 2],
        lfo_free: [0.0; 2],
        env: [[0.01, 0.1, 0.5, 0.1]; 3],
        env_gen: 0,
        panic_pole: 0.5,
    }
}

impl Dsp for Volt {
    fn activate(&mut self, sample_rate: f32) {
        self.set_rate(sample_rate);
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        if index >= COUNT || !value.is_finite() {
            return;
        }
        self.values[index] = value;
        if (id::AMP_A..=id::MOD_R).contains(&index) {
            self.env_gen = self.env_gen.wrapping_add(1);
        }
    }

    fn process(&mut self, _input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let [out_l, out_r] = output;
        let frames = out_l.len().min(out_r.len());
        out_l[..frames].fill(0.0);
        out_r[..frames].fill(0.0);
        let mut pos = 0;
        while pos < frames {
            if self.ctrl_pos == 0 {
                self.control_step();
            }
            let n = (frames - pos).min(CONTROL - self.ctrl_pos);
            for voice in self.voices.iter_mut().filter(|v| v.sounds()) {
                voice.render(
                    &self.ctl,
                    [&mut out_l[pos..pos + n], &mut out_r[pos..pos + n]],
                    n,
                );
            }
            pos += n;
            self.ctrl_pos = (self.ctrl_pos + n) % CONTROL;
        }
        for s in out_l[..frames].iter_mut().chain(&mut out_r[..frames]) {
            *s = finite(*s);
        }
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.reset();
        }
        self.stack_len = 0;
        self.wheel = 0.0;
        self.pressure = 0.0;
        self.bend = 0.0;
        self.free = [Lfo::new(SEED), Lfo::new(SEED ^ 0x5555)];
        self.ctl.lfo_free = [0.0; 2];
        self.ctrl_pos = 0;
        self.notes = 0;
        self.snap = true;
    }

    fn set_tempo(&mut self, bpm: f64) {
        if bpm.is_finite() && bpm > 0.0 {
            self.tempo = bpm;
        }
    }

    fn is_instrument(&self) -> bool {
        true
    }

    fn note_on(&mut self, key: u8, velocity: u8) {
        self.rebuild();
        let velocity = f32::from(velocity.clamp(1, 127)) / 127.0;
        match self.mode() {
            0 => self.poly_on(key, velocity),
            m => self.mono_on(key, velocity, m == 2),
        }
        self.last_pitch = f32::from(key);
    }

    fn note_off(&mut self, key: u8) {
        match self.mode() {
            0 => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.state == State::Held && v.key == key)
                {
                    v.release();
                }
            }
            m => self.mono_off(key, m == 2),
        }
    }

    fn choke_note(&mut self, key: Option<u8>, _note_id: Option<u32>, _channel: Option<u8>) {
        let Some(key) = key else {
            self.all_notes_off();
            return;
        };
        if self.mode() != 0 {
            // Remove the key from the mono stack first. If another key takes over the voice,
            // it should continue playing rather than inheriting a panic fade.
            self.mono_off(key, self.mode() == 2);
        }
        for voice in self
            .voices
            .iter_mut()
            .filter(|v| v.key == key && v.sounds())
        {
            voice.release();
            voice.panic = true;
        }
    }

    fn all_notes_off(&mut self) {
        self.stack_len = 0;
        for v in &mut self.voices {
            if v.sounds() {
                v.release();
                v.panic = true;
            }
        }
    }

    fn pitch_bend(&mut self, value: f32) {
        self.bend = value.clamp(-1.0, 1.0);
    }

    fn mod_wheel(&mut self, value: f32) {
        self.wheel = value.clamp(0.0, 1.0);
    }

    fn pressure(&mut self, value: f32) {
        self.pressure = value.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_choke_fades_only_the_target_voice_quickly() {
        let mut volt = Volt::new();
        volt.note_on(60, 100);
        volt.note_on(67, 100);
        volt.choke_note(Some(60), Some(91), None);
        let voice = |key| {
            volt.voices
                .iter()
                .find(|voice| voice.key == key && voice.sounds())
                .unwrap()
        };
        assert!(voice(60).panic);
        assert_eq!(voice(60).state, State::Released);
        assert!(!voice(67).panic);
        assert_eq!(voice(67).state, State::Held);
    }
}
