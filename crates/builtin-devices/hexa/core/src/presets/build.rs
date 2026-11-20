//! Building blocks for the factory presets.
//!
//! Operators are numbered from 1 here, like in the UI. A modulator's level is
//! its modulation index divided by [`MAX_MODULATION`](crate::MAX_MODULATION)
//! (16 rad): a level of 0.125 is an index of 2 rad.
//!
//! Operators, filters and LFOs are built as specs that carry their own
//! modulation (e.g. `.dynamic(…)`, `.cutoff_mod(…)`); placing them in a
//! patch (`patch`, `Mods::filter`, `Mods::lfo`) adds those routes to the
//! modulation matrix. Everything else is routed with `Mods::route`.

use crate::distortion::{BitcrusherParams, DistortionKind, DistortionParams};
use crate::effects::EffectKind;
use crate::envelope::EnvelopeParams;
use crate::filter::{FilterKind, FilterParams, FilterSlope};
use crate::lfo::{LfoParams, LfoWaveform};
pub use crate::modulation::ModTarget;
use crate::modulation::{ModSlot, ModSource};
use crate::operator::{ModulationMode, OPERATORS, OperatorKind, OperatorParams, PitchMode};
use crate::waveform::Waveform;
use crate::{Patch, VoiceMode};

/// Envelope with attack, decay, sustain and release; no delay or hold.
pub fn adsr(attack: f32, decay: f32, sustain: f32, release: f32) -> EnvelopeParams {
    EnvelopeParams {
        delay: 0.0,
        attack,
        hold: 0.0,
        decay,
        sustain,
        release,
        ..EnvelopeParams::default()
    }
}

/// Percussive: instant attack, decay to silence; the release matches the decay.
pub fn perc(decay: f32) -> EnvelopeParams {
    adsr(0.001, decay, 0.0, decay)
}

/// `envelope` repeating attack, hold and decay while the key is held.
pub fn looped(envelope: EnvelopeParams) -> EnvelopeParams {
    EnvelopeParams {
        looping: true,
        ..envelope
    }
}

/// Organ-like: quick attack, full sustain.
pub fn gate(attack: f32, release: f32) -> EnvelopeParams {
    adsr(attack, 0.1, 1.0, release)
}

/// An operator under construction, with the modulation it brings along.
#[derive(Clone, Copy)]
pub struct OpSpec {
    pub params: OperatorParams,
    level: Option<ModSlot>,
    fold: Option<ModSlot>,
}

impl From<OperatorParams> for OpSpec {
    fn from(params: OperatorParams) -> Self {
        Self {
            params,
            level: None,
            fold: None,
        }
    }
}

fn wave(waveform: Waveform, ratio: f32, level: f32, envelope: EnvelopeParams) -> OpSpec {
    OperatorParams {
        waveform,
        ratio,
        level,
        envelope,
        ..Default::default()
    }
    .into()
}

pub fn sine(ratio: f32, level: f32, envelope: EnvelopeParams) -> OpSpec {
    wave(Waveform::Sine, ratio, level, envelope)
}

pub fn tri(ratio: f32, level: f32, envelope: EnvelopeParams) -> OpSpec {
    wave(Waveform::Triangle, ratio, level, envelope)
}

pub fn saw(ratio: f32, level: f32, envelope: EnvelopeParams) -> OpSpec {
    wave(Waveform::Sawtooth, ratio, level, envelope)
}

/// White noise (the ratio does not matter).
pub fn noise(level: f32, envelope: EnvelopeParams) -> OpSpec {
    wave(Waveform::Noise, 1.0, level, envelope)
}

pub fn square(ratio: f32, level: f32, envelope: EnvelopeParams) -> OpSpec {
    wave(Waveform::Square, ratio, level, envelope)
}

/// Half sine (TX81Z style): the positive half of a sine, then flat.
pub fn half(ratio: f32, level: f32, envelope: EnvelopeParams) -> OpSpec {
    wave(Waveform::Half, ratio, level, envelope)
}

/// A filter operator at full level on the operators routed into it (in phase
/// mode; frequency mode moves its cutoff): cutoff `ratio` × the note,
/// `resonance` 0..=1; `.fixed(hz)` for a cutoff independent of the note.
pub fn filter_op(
    kind: OperatorKind,
    ratio: f32,
    resonance: f32,
    envelope: EnvelopeParams,
) -> OpSpec {
    OperatorParams {
        kind,
        ratio,
        resonance,
        envelope,
        ..Default::default()
    }
    .into()
}

/// Silent operator; it may still be part of the routing.
pub fn off() -> OpSpec {
    OperatorParams {
        level: 0.0,
        ..Default::default()
    }
    .into()
}

/// Chainable operator settings.
pub trait Op: Sized {
    /// Self-modulation 0..=1.
    fn fb(self, amount: f32) -> Self;
    /// How it modulates its targets.
    fn mode(self, mode: ModulationMode) -> Self;
    /// Pitch in semitones from the note instead of a ratio.
    fn semitones(self, semitones: f32) -> Self;
    /// Fixed frequency in Hz, independent of the note.
    fn fixed(self, hz: f32) -> Self;
    /// Ratio detuned by `cents`.
    fn cents(self, cents: f32) -> Self;
    /// Level `soft` at velocity 0 rising to `hard` at 127 (FM brightness by touch).
    fn dynamic(self, soft: f32, hard: f32) -> Self;
    /// Wavefolder on the operator's own waveform, 0..=1.
    fn fold(self, amount: f32) -> Self;
    fn fold_mod(self, modulation: ModSlot) -> Self;
}

impl Op for OpSpec {
    fn fb(mut self, amount: f32) -> Self {
        self.params.feedback = amount;
        self
    }
    fn mode(mut self, mode: ModulationMode) -> Self {
        self.params.mode = mode;
        self
    }
    fn semitones(mut self, semitones: f32) -> Self {
        (self.params.pitch_mode, self.params.semitones) = (PitchMode::Semitone, semitones);
        self
    }
    fn fixed(mut self, hz: f32) -> Self {
        (self.params.pitch_mode, self.params.frequency) = (PitchMode::Frequency, hz);
        self
    }
    fn cents(mut self, cents: f32) -> Self {
        self.params.ratio *= libm::exp2f(cents / 1200.0);
        self
    }
    fn dynamic(mut self, soft: f32, hard: f32) -> Self {
        self.params.level = soft;
        self.level = Some(velocity(hard - soft));
        self
    }
    fn fold(mut self, fold: f32) -> Self {
        self.params.fold = fold;
        self
    }
    fn fold_mod(mut self, modulation: ModSlot) -> Self {
        self.fold = Some(modulation);
        self
    }
}

pub const FM: ModulationMode = ModulationMode::Frequency;
pub const PWM: ModulationMode = ModulationMode::PulseWidth;
pub const SYNC: ModulationMode = ModulationMode::Sync;
pub const RING: ModulationMode = ModulationMode::Ring;
pub const LP: OperatorKind = OperatorKind::LowPass;
pub const HP: OperatorKind = OperatorKind::HighPass;
pub const BP: OperatorKind = OperatorKind::BandPass;
pub const SOFT: DistortionKind = DistortionKind::Soft;
pub const HARD: DistortionKind = DistortionKind::Hard;
pub const FOLD: DistortionKind = DistortionKind::Fold;
pub const CHORUS: EffectKind = EffectKind::Chorus;
pub const DELAY: EffectKind = EffectKind::Delay;
pub const REVERB: EffectKind = EffectKind::Reverb;
pub const MONO: VoiceMode = VoiceMode::Mono;
pub const LEGATO: VoiceMode = VoiceMode::Legato;

/// A patch from six operators, `(source, target)` modulations and heard
/// operators, all numbered from 1. Any routing works: a higher-numbered
/// source acts within the same sample, a lower one a sample later.
pub fn patch(
    specs: [OpSpec; OPERATORS],
    modulations: &[(usize, usize)],
    outputs: &[usize],
) -> Patch {
    let mut operators = specs.map(|spec| spec.params);
    for &(source, target) in modulations {
        operators[source - 1].modulates[target - 1] = true;
    }
    for &out in outputs {
        operators[out - 1].output = true;
    }
    let mut p = Patch {
        operators,
        ..Patch::default()
    };
    for (i, spec) in specs.iter().enumerate() {
        if let Some(slot) = spec.level {
            p.route(ModTarget::OpLevel(i), slot);
        }
        if let Some(slot) = spec.fold {
            p.route(ModTarget::OpFold(i), slot);
        }
    }
    p
}

pub fn slot(source: ModSource, depth: f32) -> ModSlot {
    ModSlot { source, depth }
}

pub fn velocity(depth: f32) -> ModSlot {
    slot(ModSource::Velocity, depth)
}

/// An LFO under construction, with the modulation of its amount and rate.
#[derive(Clone, Copy)]
pub struct LfoSpec {
    pub params: LfoParams,
    amount: Option<ModSlot>,
    rate: Option<ModSlot>,
}

pub fn lfo(waveform: LfoWaveform, rate: f32) -> LfoSpec {
    LfoSpec {
        params: LfoParams {
            waveform,
            rate,
            ..LfoParams::DEFAULT
        },
        amount: None,
        rate: None,
    }
}

/// Chainable LFO settings.
pub trait Lfo: Sized {
    fn fade(self, seconds: f32) -> Self;
    /// Silent for `seconds` after note-on, then the fade-in starts.
    fn delay(self, seconds: f32) -> Self;
    /// Start phase 0..1 (fraction of a period).
    fn phase(self, phase: f32) -> Self;
    /// Free running: all voices share the phase (e.g. a Leslie or chorus).
    fn free(self) -> Self;
    fn amount(self, amount: f32, modulation: ModSlot) -> Self;
    fn rate_mod(self, modulation: ModSlot) -> Self;
}

impl Lfo for LfoSpec {
    fn fade(mut self, seconds: f32) -> Self {
        self.params.fade_in = seconds;
        self
    }
    fn delay(mut self, seconds: f32) -> Self {
        self.params.delay = seconds;
        self
    }
    fn phase(mut self, phase: f32) -> Self {
        self.params.phase = phase;
        self
    }
    fn free(mut self) -> Self {
        self.params.key_sync = false;
        self
    }
    fn amount(mut self, amount: f32, modulation: ModSlot) -> Self {
        self.params.amount = amount;
        self.amount = Some(modulation);
        self
    }
    fn rate_mod(mut self, modulation: ModSlot) -> Self {
        self.rate = Some(modulation);
        self
    }
}

pub use LfoWaveform::{
    SampleHold, SawDown as LfoSawDown, SawUp as LfoSawUp, Sine as LfoSine, Square as LfoSquare,
    Triangle as LfoTriangle,
};

/// Modulators added to a patch, returning the source to connect.
pub trait Mods {
    fn lfo(&mut self, lfo: LfoSpec) -> ModSource;
    /// Routes `slot`'s source to `target` (indices from 0, like the fields).
    fn route(&mut self, target: ModTarget, slot: ModSlot);
    /// Like `route`, the depth scaled by a second source (`via`).
    fn route_via(&mut self, target: ModTarget, slot: ModSlot, via: ModSource);
    /// A filter envelope that opens further the harder you play: of the
    /// cutoff modulation `semitones` deep, 70 % always and 50 % × velocity,
    /// so a medium touch (velocity 0.6) gives the full depth and soft notes
    /// do not turn dull.
    fn touch_cutoff(&mut self, envelope: ModSource, semitones: f32);
    /// The voice filter, with its modulation.
    fn filter(&mut self, filter: FilterSpec);
    fn env(&mut self, envelope: EnvelopeParams) -> ModSource;
    /// Sine vibrato on the master tuning, `semitones` deep, fading in.
    fn vibrato(&mut self, rate: f32, semitones: f32, fade_in: f32);
    /// Depth of the modulation `source` → `target` (numbered from 1, the
    /// connection must exist), 0..=1 of the source's level.
    fn depth(&mut self, source: usize, target: usize, depth: f32);
    /// Keyboard scaling for struck and plucked sounds, the usual DX7 recipe:
    /// every operator's envelope runs faster for higher notes (`rate`), and
    /// the modulators get `treble` dB per octave above C4 (negative: softer
    /// highs). The carriers keep their level across the keyboard.
    fn struck(&mut self, rate: f32, treble: f32);
    /// One voice: `Mono` restarts the envelopes with every key, `Legato`
    /// glides between overlapping keys; `portamento` in seconds.
    fn mono(&mut self, mode: VoiceMode, portamento: f32);
    /// `voices` per key, the outer ones `detune` cents apart from the
    /// middle, spread `spread` (0..=1) across the stereo field.
    fn unison(&mut self, voices: u8, detune: f32, spread: f32);
    /// Master distortion: `drive` in dB, `tone` 0..=1 (darker), `mix`.
    fn distort(&mut self, kind: DistortionKind, drive: f32, tone: f32, mix: f32);
    /// Master bitcrusher: `bits`, held `rate` in Hz, `mix`.
    fn crush(&mut self, bits: f32, rate: f32, mix: f32);
    /// Stereo positions -1..=1 of `(operator, pan)`, numbered from 1.
    fn pan(&mut self, pans: &[(usize, f32)]);
    /// The mix of a master effect at its default settings.
    fn wet(&mut self, effect: EffectKind, mix: f32);
    /// Master delay: `time` between echoes in seconds, `feedback`, `mix`,
    /// echoes alternating left and right if `ping_pong`.
    fn echo(&mut self, time: f32, feedback: f32, mix: f32, ping_pong: bool);
    /// Master reverb: `time` (−60 dB) in seconds, `damping` 0..=1, `mix`.
    fn hall(&mut self, time: f32, damping: f32, mix: f32);
    /// Output level after the master effects (`calibrate_volumes` in
    /// `presets/mod.rs` prints it), for presets whose effects make them
    /// louder than the volume calibration (which runs without effects).
    fn output(&mut self, level: f32);
}

impl Mods for Patch {
    fn lfo(&mut self, lfo: LfoSpec) -> ModSource {
        let index = self
            .lfos
            .push(lfo.params)
            .expect("too many LFOs in a preset");
        if let Some(slot) = lfo.amount {
            self.route(ModTarget::LfoAmount(index), slot);
        }
        if let Some(slot) = lfo.rate {
            self.route(ModTarget::LfoRate(index), slot);
        }
        ModSource::Lfo(index)
    }
    fn route(&mut self, target: ModTarget, slot: ModSlot) {
        assert!(self.modulate(target, slot), "too many routes in a preset");
    }
    fn route_via(&mut self, target: ModTarget, slot: ModSlot, via: ModSource) {
        let route = crate::ModRoute {
            source: slot.source,
            target,
            depth: slot.depth,
            via,
        };
        assert!(
            self.modulation.push(route).is_some(),
            "too many routes in a preset"
        );
    }
    fn touch_cutoff(&mut self, envelope: ModSource, semitones: f32) {
        self.route(ModTarget::Cutoff(0), slot(envelope, 0.7 * semitones));
        self.route_via(
            ModTarget::Cutoff(0),
            slot(envelope, 0.5 * semitones),
            ModSource::Velocity,
        );
    }
    fn filter(&mut self, filter: FilterSpec) {
        self.filters[0] = filter.params;
        for (target, slot) in [
            (ModTarget::Cutoff(0), filter.cutoff),
            (ModTarget::Resonance(0), filter.resonance),
            (ModTarget::Drive(0), filter.drive),
        ] {
            if let Some(slot) = slot {
                self.route(target, slot);
            }
        }
    }
    fn env(&mut self, envelope: EnvelopeParams) -> ModSource {
        ModSource::Envelope(
            self.envelopes
                .push(envelope)
                .expect("too many envelopes in a preset"),
        )
    }
    fn vibrato(&mut self, rate: f32, semitones: f32, fade_in: f32) {
        let source = self.lfo(lfo(LfoWaveform::Sine, rate).fade(fade_in));
        self.route(ModTarget::Tune, slot(source, semitones));
    }
    fn mono(&mut self, mode: VoiceMode, portamento: f32) {
        self.voice_mode = mode;
        self.portamento = portamento;
    }
    fn distort(&mut self, kind: DistortionKind, drive: f32, tone: f32, mix: f32) {
        self.effects.distortion = DistortionParams {
            mix,
            drive,
            kind,
            tone,
        };
    }
    fn crush(&mut self, bits: f32, rate: f32, mix: f32) {
        self.effects.bitcrusher = BitcrusherParams { mix, bits, rate };
    }
    fn pan(&mut self, pans: &[(usize, f32)]) {
        for &(op, pan) in pans {
            self.operators[op - 1].pan = pan;
        }
    }
    fn wet(&mut self, effect: EffectKind, mix: f32) {
        let e = &mut self.effects;
        match effect {
            EffectKind::Distortion => e.distortion.mix = mix,
            EffectKind::Bitcrusher => e.bitcrusher.mix = mix,
            EffectKind::Eq => panic!("the EQ has no mix"),
            EffectKind::Chorus => e.chorus.mix = mix,
            EffectKind::Delay => e.delay.mix = mix,
            EffectKind::Reverb => e.reverb.mix = mix,
        }
    }
    fn echo(&mut self, time: f32, feedback: f32, mix: f32, ping_pong: bool) {
        let d = &mut self.effects.delay;
        (d.time, d.feedback, d.mix, d.ping_pong) = (time, feedback, mix, ping_pong);
    }
    fn hall(&mut self, time: f32, damping: f32, mix: f32) {
        let r = &mut self.effects.reverb;
        (r.time, r.damping, r.mix) = (time, damping, mix);
    }
    fn output(&mut self, level: f32) {
        self.effects.level = level;
    }
    fn unison(&mut self, voices: u8, detune: f32, spread: f32) {
        self.unison = voices;
        self.detune = detune;
        self.unison_spread = spread;
    }
    fn struck(&mut self, rate: f32, treble: f32) {
        for i in 0..self.operators.len() {
            let modulates =
                (0..self.operators.len()).any(|t| t != i && self.operators[i].modulates[t]);
            let op = &mut self.operators[i];
            op.key_scaling.rate = rate;
            if modulates {
                op.key_scaling.right = treble;
            }
        }
    }
    fn depth(&mut self, source: usize, target: usize, depth: f32) {
        let op = &mut self.operators[source - 1];
        assert!(
            op.modulates[target - 1],
            "no modulation {source} → {target}"
        );
        op.depths[target - 1] = depth;
    }
}

/// A voice filter under construction, with its modulation.
#[derive(Clone, Copy)]
pub struct FilterSpec {
    pub params: FilterParams,
    cutoff: Option<ModSlot>,
    resonance: Option<ModSlot>,
    drive: Option<ModSlot>,
}

fn filter(kind: FilterKind, cutoff: f32, resonance: f32) -> FilterSpec {
    let params = FilterParams {
        kind,
        cutoff,
        resonance,
        ..FilterParams::DEFAULT
    };
    FilterSpec {
        params,
        cutoff: None,
        resonance: None,
        drive: None,
    }
}

pub fn lowpass(cutoff: f32, resonance: f32) -> FilterSpec {
    filter(FilterKind::LowPass, cutoff, resonance)
}

pub fn highpass(cutoff: f32, resonance: f32) -> FilterSpec {
    filter(FilterKind::HighPass, cutoff, resonance)
}

pub fn bandpass(cutoff: f32, resonance: f32) -> FilterSpec {
    filter(FilterKind::BandPass, cutoff, resonance)
}

/// Chainable filter settings.
pub trait Filter: Sized {
    fn db24(self) -> Self;
    fn drive(self, drive: f32) -> Self;
    fn keytrack(self, keytrack: f32) -> Self;
    /// Cutoff modulation in semitones.
    fn cutoff_mod(self, slot: ModSlot) -> Self;
    fn resonance_mod(self, slot: ModSlot) -> Self;
    fn drive_mod(self, slot: ModSlot) -> Self;
}

impl Filter for FilterSpec {
    fn db24(mut self) -> Self {
        self.params.slope = FilterSlope::Db24;
        self
    }
    fn drive(mut self, drive: f32) -> Self {
        self.params.drive = drive;
        self
    }
    fn keytrack(mut self, keytrack: f32) -> Self {
        self.params.keytrack = keytrack;
        self
    }
    fn cutoff_mod(mut self, slot: ModSlot) -> Self {
        self.cutoff = Some(slot);
        self
    }
    fn resonance_mod(mut self, slot: ModSlot) -> Self {
        self.resonance = Some(slot);
        self
    }
    fn drive_mod(mut self, slot: ModSlot) -> Self {
        self.drive = Some(slot);
        self
    }
}
