//! The patch: everything that defines a sound.

use crate::effects::EffectParams;
use crate::envelope::EnvelopeParams;
use crate::filter::{FILTERS, FilterKind, FilterParams};
use crate::lfo::LfoBank;
use crate::mod_envelope::EnvelopeBank;
use crate::modulation::{ModCounts, ModMatrix, ModRoute, ModSlot, ModTarget, Usage};
use crate::operator::{OPERATORS, OperatorParams};
use crate::voicing::{MAX_PORTAMENTO, MAX_UNISON, VoiceMode};

/// Largest unison detune in cents (the outer voices, either way).
pub const MAX_DETUNE: f32 = 100.0;
/// Largest master tuning in semitones, up and down.
pub const MAX_TUNE: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Patch {
    pub operators: [OperatorParams; OPERATORS],
    /// Master gain applied to the voice sum.
    pub volume: f32,
    /// Master balance per voice, after the filters: -1 = left only, 0 =
    /// both at full level, 1 = right only.
    #[cfg_attr(feature = "serde", serde(default))]
    pub balance: f32,
    /// Master tuning in semitones, ±[`MAX_TUNE`]. Applies to operators that
    /// follow the note ([`PitchMode::Ratio`] and [`PitchMode::Semitone`]).
    #[cfg_attr(feature = "serde", serde(default))]
    pub tune: f32,
    /// LFOs available as modulation sources.
    #[cfg_attr(feature = "serde", serde(default))]
    pub lfos: LfoBank,
    /// Modulation envelopes available as sources.
    #[cfg_attr(feature = "serde", serde(default))]
    pub envelopes: EnvelopeBank,
    /// The modulation matrix: which sources move which parameters, see
    /// [`crate::modulation`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub modulation: ModMatrix,
    /// Voice filters, in series after the heard operators.
    #[cfg_attr(feature = "serde", serde(default))]
    pub filters: [FilterParams; FILTERS],
    /// Master effects on the sum of the voices; applied by [`crate::Synth`],
    /// not by [`Engine`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub effects: EffectParams,
    /// Poly, mono or legato, see [`VoiceMode`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub voice_mode: VoiceMode,
    /// Glide time between notes in seconds, 0..=[`MAX_PORTAMENTO`]; 0 = off.
    #[cfg_attr(feature = "serde", serde(default))]
    pub portamento: f32,
    /// Voices per key, 1..=[`MAX_UNISON`]; they share the 16 voices.
    #[cfg_attr(feature = "serde", serde(default = "single"))]
    pub unison: u8,
    /// Unison detune in cents: the outer voices of a key are this far up
    /// and down, the others evenly in between; 0..=[`MAX_DETUNE`].
    #[cfg_attr(feature = "serde", serde(default = "default_detune"))]
    pub detune: f32,
    /// 0..=1: how far the unison voices spread across the stereo field.
    #[cfg_attr(feature = "serde", serde(default = "default_spread"))]
    pub unison_spread: f32,
}

#[cfg(feature = "serde")]
fn single() -> u8 {
    1
}

#[cfg(feature = "serde")]
fn default_detune() -> f32 {
    Patch::default().detune
}

#[cfg(feature = "serde")]
fn default_spread() -> f32 {
    Patch::default().unison_spread
}

impl Patch {
    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults. Use this for patches from untrusted sources.
    pub fn sanitized(self) -> Self {
        let counts = ModCounts {
            lfos: self.lfos.len(),
            envelopes: self.envelopes.len(),
        };
        Self {
            operators: self.operators.map(OperatorParams::sanitized),
            volume: crate::clamp_or(self.volume, 0.0, 1.0, Self::default().volume),
            balance: crate::clamp_or(self.balance, -1.0, 1.0, 0.0),
            tune: crate::clamp_or(self.tune, -MAX_TUNE, MAX_TUNE, 0.0),
            lfos: self.lfos.map(|lfo| lfo.sanitized()),
            envelopes: self.envelopes.map(EnvelopeParams::sanitized),
            modulation: self.modulation.map(|route| route.sanitized(counts)),
            filters: self.filters.map(FilterParams::sanitized),
            effects: self.effects.sanitized(),
            portamento: crate::clamp_or(self.portamento, 0.0, MAX_PORTAMENTO, 0.0),
            unison: self.unison.clamp(1, MAX_UNISON as u8),
            detune: crate::clamp_or(self.detune, 0.0, MAX_DETUNE, Self::default().detune),
            unison_spread: crate::clamp_or(
                self.unison_spread,
                0.0,
                1.0,
                Self::default().unison_spread,
            ),
            ..self
        }
    }

    /// The unison spread in effect: none without unison.
    pub(crate) fn spread(&self) -> f32 {
        if self.unison > 1 {
            self.unison_spread
        } else {
            0.0
        }
    }

    /// Adds a route moving `target` by `slot`'s source and depth; `false`
    /// if the matrix is full.
    pub fn modulate(&mut self, target: ModTarget, slot: ModSlot) -> bool {
        let route = ModRoute {
            source: slot.source,
            target,
            depth: slot.depth,
            ..ModRoute::default()
        };
        self.modulation.push(route).is_some()
    }

    /// Modulators that something uses; only these are computed. An envelope
    /// that only modulates an unused LFO, or a filter that is off, does not
    /// count.
    pub(crate) fn usage(&self) -> Usage {
        let below = |n: usize| ((1u16 << n) - 1) as u8;
        let routes = self
            .modulation
            .as_slice()
            .iter()
            .filter(|r| match r.target {
                ModTarget::Cutoff(f) | ModTarget::Resonance(f) | ModTarget::Drive(f) => self
                    .filters
                    .get(f)
                    .is_some_and(|filter| filter.kind != FilterKind::Off),
                _ => true,
            });
        let (params, _) = crate::modulation::usage(routes.clone(), 0);
        let lfos = params.lfos & below(self.lfos.len());
        let (_, by_lfos) = crate::modulation::usage(routes, lfos);
        Usage {
            lfos,
            envelopes: (params.envelopes | by_lfos.envelopes) & below(self.envelopes.len()),
        }
    }
}

impl Default for Patch {
    /// Operator 2 modulates operator 1, which is heard; the other operators
    /// are unused.
    fn default() -> Self {
        let mut operators = [OperatorParams {
            level: 0.0,
            ..Default::default()
        }; OPERATORS];
        operators[0] = OperatorParams {
            output: true,
            ..Default::default()
        };
        operators[1] = OperatorParams {
            // 2 rad modulation index.
            level: 0.125,
            modulates: core::array::from_fn(|i| i == 0),
            envelope: EnvelopeParams {
                decay: 0.5,
                sustain: 0.3,
                ..Default::default()
            },
            ..Default::default()
        };
        Self {
            operators,
            volume: 0.25,
            balance: 0.0,
            tune: 0.0,
            lfos: LfoBank::new(),
            envelopes: EnvelopeBank::new(),
            modulation: ModMatrix::new(),
            filters: [FilterParams::DEFAULT; FILTERS],
            effects: EffectParams::default(),
            voice_mode: VoiceMode::Poly,
            portamento: 0.0,
            unison: 1,
            detune: 20.0,
            unison_spread: 1.0,
        }
    }
}
