//! Modulation of parameters by sources: the modulation matrix.
//!
//! Every modulatable parameter has a base value (its slider). The patch's
//! [`ModRoute`]s (up to [`MAX_ROUTES`]) each move one [`ModTarget`] by
//! `depth × source × via` (via: a second source scaling the depth, 1 when
//! unset). All routes on a target add up, and the sum is clamped once to the
//! parameter's range: `clamp(base + Σ depth × source × via)`. LFOs are
//! bipolar (-1..=1, around the base value), envelopes unipolar (0..=1, from
//! the base value in the direction of the depth).
//!
//! Sources are evaluated per voice at control rate into [`SourceValues`], in
//! stages so nothing depends on itself: envelopes first (no inputs), then
//! LFOs (whose amount and rate may follow everything but LFOs), then the
//! parameters (any source). The MIDI controllers (mod wheel, pitch bend,
//! aftertouch), the key and a random value per note need no computation.
//! New kinds of sources are new [`ModSource`] variants (appended, so saved
//! patches keep their meaning) plus a field in [`SourceValues`].

use crate::filter::{FILTERS, MAX_CUTOFF_DEPTH};
use crate::lfo::{MAX_LFOS, MAX_RATE_DEPTH};
use crate::mod_envelope::MAX_ENVELOPES;
use crate::operator::OPERATORS;

/// Largest pitch modulation depth in semitones.
pub const MAX_PITCH_DEPTH: f32 = 48.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ModSource {
    /// Not modulated: the base value applies.
    #[default]
    None,
    /// LFO by index into the patch's LFO list; bipolar, -1..=1.
    Lfo(usize),
    /// Modulation envelope by index into the patch's envelope list;
    /// unipolar, 0..=1.
    Envelope(usize),
    /// The note's velocity, unipolar 0..=1 (linear, 127 = 1).
    Velocity,
    /// Key pressure, unipolar 0..=1: the stronger of channel pressure and
    /// the pressure of the voice's own key (polyphonic aftertouch).
    Aftertouch,
    /// The mod wheel (controller 1; the front panel's MOD strip), 0..=1.
    ModWheel,
    /// The pitch wheel (the PITCH strip), bipolar -1..=1. It bends nothing
    /// by itself: the factory presets route it to [`ModTarget::Tune`].
    PitchBend,
    /// The voice's pitch relative to middle C (60), bipolar: ±1 at ±64
    /// semitones (clamped), so a depth of 64 semitones on a pitch tracks
    /// the keys 1:1.
    KeyTrack,
    /// A random value per note, bipolar -1..=1, drawn at note-on.
    Random,
}

/// How many modulators of each kind a patch has, for sanitizing slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModCounts {
    pub lfos: usize,
    pub envelopes: usize,
}

/// Which modulators are needed, as bit masks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub(crate) lfos: u8,
    pub(crate) envelopes: u8,
}

impl core::ops::BitOr for Usage {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self {
            lfos: self.lfos | other.lfos,
            envelopes: self.envelopes | other.envelopes,
        }
    }
}

impl Usage {
    pub(crate) fn is_empty(&self) -> bool {
        self.lfos == 0 && self.envelopes == 0
    }
}

/// A source and a depth, for building routes (see [`crate::Patch::modulate`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct ModSlot {
    pub source: ModSource,
    /// Deviation at full source output, in the parameter's unit (semitones
    /// for pitch, 0..1 fractions for levels). Negative inverts the source.
    pub depth: f32,
}

impl ModSlot {
    pub const NONE: Self = Self {
        source: ModSource::None,
        depth: 0.0,
    };

    /// Clamps the depth to ±`max_depth`; sources that do not exist (an index
    /// beyond `counts`) become `None`.
    pub fn sanitized(self, max_depth: f32, counts: ModCounts) -> Self {
        let source = match self.source {
            ModSource::Lfo(i) if i >= counts.lfos => ModSource::None,
            ModSource::Envelope(i) if i >= counts.envelopes => ModSource::None,
            other => other,
        };
        Self {
            source,
            depth: crate::clamp_or(self.depth, -max_depth, max_depth, 0.0),
        }
    }

    /// Whether the slot changes anything.
    pub fn is_active(&self) -> bool {
        self.source != ModSource::None && self.depth != 0.0
    }

    /// Modulators this slot needs.
    pub(crate) fn usage(&self) -> Usage {
        match self.source {
            ModSource::Lfo(i) if self.is_active() && i < MAX_LFOS => Usage {
                lfos: 1 << i,
                envelopes: 0,
            },
            ModSource::Envelope(i) if self.is_active() && i < MAX_ENVELOPES => Usage {
                lfos: 0,
                envelopes: 1 << i,
            },
            _ => Usage::default(),
        }
    }
}

const _: () = assert!(
    MAX_LFOS <= 8 && MAX_ENVELOPES <= 8,
    "modulator masks are u8"
);

/// Largest number of routes in a patch's modulation matrix.
pub const MAX_ROUTES: usize = 32;

/// The modulation matrix of a patch.
pub type ModMatrix = crate::bank::Bank<ModRoute, MAX_ROUTES>;

/// A modulatable parameter. Indices are 0-based (operator, filter, LFO).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ModTarget {
    /// Unassigned: the route does nothing.
    #[default]
    None,
    /// Operator pitch in semitones (the cutoff of a filter operator), in
    /// every pitch mode.
    OpPitch(usize),
    /// Operator level, 0..=1.
    OpLevel(usize),
    /// Operator feedback, 0..=1.
    OpFeedback(usize),
    /// Operator balance, -1..=1.
    OpPan(usize),
    /// Operator wavefolder, 0..=1.
    OpFold(usize),
    /// Master volume, 0..=1 (e.g. tremolo).
    Volume,
    /// Master tuning in semitones (e.g. vibrato).
    Tune,
    /// Master balance, -1..=1 (e.g. auto-pan).
    Balance,
    /// Filter cutoff in semitones.
    Cutoff(usize),
    /// Filter resonance, 0..=1.
    Resonance(usize),
    /// Filter drive, 0..=1.
    Drive(usize),
    /// LFO amount, 0..=1; only envelopes and velocity modulate LFOs.
    LfoAmount(usize),
    /// LFO rate in octaves; only envelopes and velocity modulate LFOs.
    LfoRate(usize),
    /// Operator frequency as a factor: × (1 + offset), -1..=1 (the display's
    /// FINE; floored at 0, a stopped oscillator).
    OpRatio(usize),
    /// Stage `.1` ([`ENVELOPE_STAGES`]: 0 delay, 1 attack, 2 hold, 3 decay,
    /// 4 sustain, 5 release) of operator `.0`'s envelope. Times move on
    /// the cubic scale of [`time_offset`] (fractions of it), the sustain
    /// level directly; continuously, the running stage speeds up or slows
    /// down.
    OpEnvelope(usize, usize),
    /// Stage `.1` of modulation envelope `.0`, as [`ModTarget::OpEnvelope`].
    /// Envelopes run before the LFOs and other envelopes, so neither
    /// modulates them.
    ModEnvelope(usize, usize),
    /// LFO fade-in time, on the cubic scale of [`time_offset`] up to
    /// [`crate::lfo::MAX_FADE_IN`]; like the LFO's other targets not by LFOs.
    LfoFade(usize),
    /// Glide time, on the cubic scale of [`time_offset`] up to
    /// [`crate::MAX_PORTAMENTO`].
    Glide,
    /// The depth (0..=1) with which operator `.0` modulates operator `.1`;
    /// only routes the patch has. Not on the operator itself (that is
    /// [`ModTarget::OpFeedback`]).
    OpDepth(usize, usize),
}

/// Stages of an envelope a target can address.
pub const ENVELOPE_STAGES: usize = 6;

/// Number of distinct targets, the length of [`Offsets`].
pub(crate) const TARGETS: usize = target_base::DEPTH + OPERATORS * (OPERATORS - 1);

/// First index of each kind of target.
mod target_base {
    use super::*;

    pub(super) const FILTER: usize = 5 * OPERATORS + 3;
    pub(super) const LFO: usize = FILTER + 3 * FILTERS;
    pub(super) const RATIO: usize = LFO + 2 * MAX_LFOS;
    pub(super) const OP_ENVELOPE: usize = RATIO + OPERATORS;
    pub(super) const MOD_ENVELOPE: usize = OP_ENVELOPE + ENVELOPE_STAGES * OPERATORS;
    pub(super) const FADE: usize = MOD_ENVELOPE + ENVELOPE_STAGES * MAX_ENVELOPES;
    pub(super) const GLIDE: usize = FADE + MAX_LFOS;
    pub(super) const DEPTH: usize = GLIDE + 1;
}

/// A time `base` moved by `offset` on the cubic scale up to `max` (the
/// display's 0–99 scale for times): t = max × p³, p moved by `offset` and
/// clamped to 0..=1. Unlike a factor it also moves a time of 0.
#[inline]
pub fn time_offset(base: f32, offset: f32, max: f32) -> f32 {
    if offset == 0.0 {
        return base;
    }
    let p = (libm::cbrtf((base / max).max(0.0)) + offset).clamp(0.0, 1.0);
    max * p * p * p
}

impl ModTarget {
    /// Dense index 0..[`TARGETS`]; `None` for no target or an index out of
    /// range.
    #[inline]
    pub(crate) fn index(self) -> Option<usize> {
        use target_base::*;
        let at = |base: usize, i: usize, n: usize| (i < n).then_some(base + i);
        let stage = |base: usize, i: usize, n: usize, s: usize| {
            (i < n && s < ENVELOPE_STAGES).then_some(base + i * ENVELOPE_STAGES + s)
        };
        match self {
            Self::None => None,
            Self::OpPitch(i) => at(0, i, OPERATORS),
            Self::OpLevel(i) => at(OPERATORS, i, OPERATORS),
            Self::OpFeedback(i) => at(2 * OPERATORS, i, OPERATORS),
            Self::OpPan(i) => at(3 * OPERATORS, i, OPERATORS),
            Self::OpFold(i) => at(4 * OPERATORS, i, OPERATORS),
            Self::Volume => Some(5 * OPERATORS),
            Self::Tune => Some(5 * OPERATORS + 1),
            Self::Balance => Some(5 * OPERATORS + 2),
            Self::Cutoff(i) => at(FILTER, i, FILTERS),
            Self::Resonance(i) => at(FILTER + FILTERS, i, FILTERS),
            Self::Drive(i) => at(FILTER + 2 * FILTERS, i, FILTERS),
            Self::LfoAmount(i) => at(LFO, i, MAX_LFOS),
            Self::LfoRate(i) => at(LFO + MAX_LFOS, i, MAX_LFOS),
            Self::OpRatio(i) => at(RATIO, i, OPERATORS),
            Self::OpEnvelope(i, s) => stage(OP_ENVELOPE, i, OPERATORS, s),
            Self::ModEnvelope(e, s) => stage(MOD_ENVELOPE, e, MAX_ENVELOPES, s),
            Self::LfoFade(i) => at(FADE, i, MAX_LFOS),
            Self::Glide => Some(GLIDE),
            Self::OpDepth(from, to) if from != to && from < OPERATORS && to < OPERATORS => {
                // Dense without the diagonal (feedback).
                Some(DEPTH + from * (OPERATORS - 1) + to - usize::from(to > from))
            }
            Self::OpDepth(..) => None,
        }
    }

    /// Largest depth, in the parameter's unit.
    pub fn max_depth(self) -> f32 {
        match self {
            Self::OpPitch(_) | Self::Tune => MAX_PITCH_DEPTH,
            Self::OpPan(_) | Self::Balance => 2.0,
            Self::Cutoff(_) => MAX_CUTOFF_DEPTH,
            Self::LfoRate(_) => MAX_RATE_DEPTH,
            _ => 1.0,
        }
    }

    /// The LFO this target belongs to, if any: such targets are summed
    /// before the LFOs run, so LFOs do not modulate them.
    fn lfo(self) -> Option<usize> {
        match self {
            Self::LfoAmount(l) | Self::LfoRate(l) | Self::LfoFade(l) => Some(l),
            _ => None,
        }
    }

    /// The modulation envelope this target belongs to, if any: such
    /// targets are summed before the envelopes run, so neither envelopes
    /// nor LFOs modulate them.
    fn envelope(self) -> Option<usize> {
        match self {
            Self::ModEnvelope(e, _) => Some(e),
            _ => None,
        }
    }
}

/// Which LFO parameters are modulated, as bit masks of LFOs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LfoModulation {
    pub(crate) rates: u8,
    pub(crate) amounts: u8,
    pub(crate) fades: u8,
}

/// One entry of the modulation matrix: `target` moves by
/// `depth × source × via` (via unset = 1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct ModRoute {
    pub source: ModSource,
    pub target: ModTarget,
    /// Deviation at full source (and via) output, in the target's unit
    /// (semitones for pitch and cutoff, fractions for levels); negative
    /// inverts. At most ±[`ModTarget::max_depth`].
    pub depth: f32,
    /// Optional second source scaling the depth (e.g. velocity: more
    /// vibrato on hard notes).
    pub via: ModSource,
}

impl ModRoute {
    /// Clamps the depth for the target; sources and targets that do not
    /// exist become `None`, as do LFOs modulating LFOs. Kept in place, so a
    /// matrix row stays where it is.
    pub fn sanitized(self, counts: ModCounts) -> Self {
        let early = self.target.lfo().is_some() || self.target.envelope().is_some();
        let exists = |source: ModSource| match source {
            ModSource::Lfo(i) if i >= counts.lfos => ModSource::None,
            ModSource::Envelope(i) if i >= counts.envelopes => ModSource::None,
            ModSource::Lfo(_) if early => ModSource::None,
            ModSource::Envelope(_) if self.target.envelope().is_some() => ModSource::None,
            other => other,
        };
        let target = match self.target {
            target if target.index().is_none() => ModTarget::None,
            target if target.lfo().is_some_and(|l| l >= counts.lfos) => ModTarget::None,
            target if target.envelope().is_some_and(|e| e >= counts.envelopes) => ModTarget::None,
            target => target,
        };
        let max = target.max_depth();
        Self {
            source: exists(self.source),
            target,
            depth: crate::clamp_or(self.depth, -max, max, 0.0),
            via: exists(self.via),
        }
    }

    /// Whether the route changes anything.
    pub fn is_active(&self) -> bool {
        self.source != ModSource::None && self.target.index().is_some() && self.depth != 0.0
    }

    /// Modulators this route needs (its source and via).
    fn usage(&self) -> Usage {
        if !self.is_active() {
            return Usage::default();
        }
        let of = |source: ModSource| ModSlot { source, depth: 1.0 }.usage();
        of(self.source) | of(self.via)
    }
}

/// What a patch's matrix modulates, as a bit set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modulated {
    /// Bit [`ModTarget::index`]: some active route moves this target.
    targets: [u64; TARGETS.div_ceil(64)],
    /// Derived once per patch, see the methods of the same names.
    op_envelopes: u8,
    mod_envelopes: u8,
    depths: bool,
    ratios: u8,
    lfo_fades: u8,
    lfo_rates: u8,
    lfo_amounts: u8,
    glide: bool,
}

impl Modulated {
    pub(crate) fn new(routes: &[ModRoute]) -> Self {
        let mut this = Self::default();
        for i in routes
            .iter()
            .filter(|r| r.is_active())
            .filter_map(|r| r.target.index())
        {
            this.targets[i / 64] |= 1 << (i % 64);
        }
        for i in 0..OPERATORS {
            if (0..ENVELOPE_STAGES).any(|s| this.has(ModTarget::OpEnvelope(i, s))) {
                this.op_envelopes |= 1 << i;
            }
        }
        for e in 0..MAX_ENVELOPES {
            if (0..ENVELOPE_STAGES).any(|s| this.has(ModTarget::ModEnvelope(e, s))) {
                this.mod_envelopes |= 1 << e;
            }
        }
        this.depths =
            (0..OPERATORS).any(|a| (0..OPERATORS).any(|b| this.has(ModTarget::OpDepth(a, b))));
        this.ratios = (0..OPERATORS)
            .filter(|&i| this.has(ModTarget::OpRatio(i)))
            .fold(0, |m, i| m | 1 << i);
        let lfos = |this: &Self, target: fn(usize) -> ModTarget| {
            (0..MAX_LFOS)
                .filter(|&l| this.has(target(l)))
                .fold(0, |m, l| m | 1 << l)
        };
        this.lfo_fades = lfos(&this, ModTarget::LfoFade);
        this.lfo_rates = lfos(&this, ModTarget::LfoRate);
        this.lfo_amounts = lfos(&this, ModTarget::LfoAmount);
        this.glide = this.has(ModTarget::Glide);
        this
    }

    /// Operators (bit mask) whose frequency factor is modulated.
    pub(crate) fn ratios(&self) -> u8 {
        self.ratios
    }

    /// LFOs (bit masks) whose rate, amount and fade-in are modulated.
    pub(crate) fn lfos(&self) -> LfoModulation {
        LfoModulation {
            rates: self.lfo_rates,
            amounts: self.lfo_amounts,
            fades: self.lfo_fades,
        }
    }

    /// Whether the glide time is modulated.
    pub(crate) fn glide(&self) -> bool {
        self.glide
    }

    /// Whether an active route moves `target`.
    #[inline]
    pub(crate) fn has(&self, target: ModTarget) -> bool {
        target
            .index()
            .is_some_and(|i| self.targets[i / 64] & (1 << (i % 64)) != 0)
    }

    /// Operators (bit mask) with a modulated envelope stage.
    pub(crate) fn op_envelopes(&self) -> u8 {
        self.op_envelopes
    }

    /// Modulation envelopes (bit mask) with a modulated stage.
    pub(crate) fn mod_envelopes(&self) -> u8 {
        self.mod_envelopes
    }

    /// Whether any operator's modulation depth is modulated.
    pub(crate) fn depths(&self) -> bool {
        self.depths
    }
}

/// Modulators that the routes on parameters use (all but LFO targets), and
/// those that the routes on the LFOs in `lfos` (bit mask) use.
pub(crate) fn usage<'a>(
    routes: impl IntoIterator<Item = &'a ModRoute>,
    lfos: u8,
) -> (Usage, Usage) {
    routes.into_iter().fold(
        (Usage::default(), Usage::default()),
        |(params, by_lfos), r| match r.target.lfo() {
            None => (params | r.usage(), by_lfos),
            Some(l) if l < MAX_LFOS && lfos & (1 << l) != 0 => (params, by_lfos | r.usage()),
            Some(_) => (params, by_lfos),
        },
    )
}

/// A route ready for evaluation: its target as its patch's slot (see
/// [`Slots`]).
#[derive(Clone, Copy, Debug, Default)]
struct Compiled {
    slot: u8,
    source: ModSource,
    via: ModSource,
    depth: f32,
}

/// Which targets a patch modulates: each has a slot in [`Offsets`], at most
/// one per route, so offsets stay small however many targets exist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Slots([u8; TARGETS]);

impl Slots {
    /// No target has a slot.
    pub(crate) const EMPTY: Self = Self([Self::NONE; TARGETS]);
    /// The slot of targets without routes: one past the routes' slots,
    /// always 0, so reading needs no branch.
    const NONE: u8 = MAX_ROUTES as u8;
}

/// A patch's active routes, prepared once per patch: the targets' slots,
/// in three parts that are summed at different times: the routes on the
/// modulation envelopes' parameters (before the envelopes run), on the
/// LFOs' (before the LFOs run), on everything else; each part in matrix
/// order (so the sums are the same as over the whole matrix).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Routes {
    list: [Compiled; MAX_ROUTES],
    len: usize,
    /// `list[..envelope]` move envelope parameters, `list[envelope..lfo]`
    /// LFO parameters, `list[lfo..len]` the others.
    envelope: usize,
    lfo: usize,
    slots: Slots,
}

const _: () = assert!(MAX_ROUTES < u8::MAX as usize, "slots are u8");

impl Routes {
    pub(crate) fn new(routes: &[ModRoute]) -> Self {
        let mut this = Self {
            list: [Compiled::default(); MAX_ROUTES],
            len: 0,
            envelope: 0,
            lfo: 0,
            slots: Slots::EMPTY,
        };
        let part = |r: &ModRoute| match (r.target.envelope(), r.target.lfo()) {
            (Some(_), _) => 0,
            (None, Some(_)) => 1,
            (None, None) => 2,
        };
        let mut used = 0;
        for p in 0..3 {
            for r in routes.iter().filter(|r| r.is_active() && part(r) == p) {
                let (Some(index), Some(entry)) = (r.target.index(), this.list.get_mut(this.len))
                else {
                    continue;
                };
                if this.slots.0[index] == Slots::NONE {
                    this.slots.0[index] = used;
                    used += 1;
                }
                *entry = Compiled {
                    slot: this.slots.0[index],
                    source: r.source,
                    via: r.via,
                    depth: r.depth,
                };
                this.len += 1;
            }
            match p {
                0 => this.envelope = this.len,
                1 => this.lfo = this.len,
                _ => {}
            }
        }
        this
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The targets' slots, for reading [`Offsets`].
    pub(crate) fn slots(&self) -> &Slots {
        &self.slots
    }

    /// Offsets of the modulation envelopes' parameters (sources: neither
    /// envelopes nor LFOs). `slots` must be this patch's [`Self::slots`].
    pub(crate) fn envelope_offsets<'a>(
        &self,
        slots: &'a Slots,
        values: &SourceValues,
    ) -> Offsets<'a> {
        Offsets::sum(&self.list[..self.envelope], slots, values)
    }

    /// Offsets of the LFOs' own parameters (sources: anything but LFOs).
    pub(crate) fn lfo_offsets<'a>(&self, slots: &'a Slots, values: &SourceValues) -> Offsets<'a> {
        Offsets::sum(&self.list[self.envelope..self.lfo], slots, values)
    }

    /// Offsets of every other parameter.
    pub(crate) fn offsets<'a>(&self, slots: &'a Slots, values: &SourceValues) -> Offsets<'a> {
        Offsets::sum(&self.list[self.lfo..self.len], slots, values)
    }
}

/// Sum of the routes' deviations per target, for one voice at one point in
/// time: one value per slot. Read with [`Offsets::get`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Offsets<'a> {
    /// Per slot; the last one ([`Slots::NONE`]) stays 0.
    values: [f32; MAX_ROUTES + 1],
    slots: &'a Slots,
}

impl Offsets<'static> {
    /// No deviation anywhere.
    pub(crate) const ZERO: Self = Self {
        values: [0.0; MAX_ROUTES + 1],
        slots: &Slots::EMPTY,
    };

    /// All of `routes`' summed deviations with the sources at `values`.
    #[cfg(test)]
    pub(crate) fn new(routes: &[ModRoute], values: &SourceValues) -> Self {
        let routes = Routes::new(routes);
        let slots: &'static Slots = Box::leak(Box::new(routes.slots));
        let mut offsets = routes.envelope_offsets(slots, values);
        for part in [
            routes.lfo_offsets(slots, values),
            routes.offsets(slots, values),
        ] {
            for (sum, other) in offsets.values.iter_mut().zip(part.values) {
                *sum += other;
            }
        }
        offsets
    }
}

impl<'a> Offsets<'a> {
    #[inline]
    fn sum(routes: &[Compiled], slots: &'a Slots, values: &SourceValues) -> Self {
        let mut offsets = Self {
            values: [0.0; MAX_ROUTES + 1],
            slots,
        };
        for route in routes {
            let mut value = values.value(route.source);
            if route.via != ModSource::None {
                value *= values.value(route.via);
            }
            offsets.values[route.slot as usize] += route.depth * value;
        }
        offsets
    }

    /// Deviation of `target` from its base value.
    #[inline]
    pub(crate) fn get(&self, target: ModTarget) -> f32 {
        target
            .index()
            .map_or(0.0, |i| self.values[self.slots.0[i] as usize])
    }
}

/// The summed modulation of one voice at the end of its last block, for
/// displays (see [`crate::Synth::modulation`]): deviations from the base
/// values, in the targets' units, before clamping to their ranges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModulationValues {
    values: [f32; MAX_ROUTES + 1],
    slots: Slots,
}

impl ModulationValues {
    /// No modulation anywhere.
    pub const NONE: Self = Self {
        values: [0.0; MAX_ROUTES + 1],
        slots: Slots::EMPTY,
    };

    /// Deviation of `target` from its base value; 0 if nothing moves it.
    pub fn get(&self, target: ModTarget) -> f32 {
        Offsets {
            values: self.values,
            slots: &self.slots,
        }
        .get(target)
    }

    /// As 32-bit words, to pass between cores through atomics.
    pub fn to_words(&self) -> [u32; MODULATION_WORDS] {
        let mut words = [0; MODULATION_WORDS];
        for (w, v) in words.iter_mut().zip(&self.values) {
            *w = v.to_bits();
        }
        for (i, chunk) in self.slots.0.chunks(4).enumerate() {
            let mut bytes = [0; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            words[MAX_ROUTES + 1 + i] = u32::from_le_bytes(bytes);
        }
        words
    }

    /// Back from [`Self::to_words`].
    pub fn from_words(words: &[u32; MODULATION_WORDS]) -> Self {
        let mut this = Self::NONE;
        for (v, w) in this.values.iter_mut().zip(words) {
            *v = f32::from_bits(*w);
        }
        for (i, chunk) in this.slots.0.chunks_mut(4).enumerate() {
            let bytes = words[MAX_ROUTES + 1 + i].to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        // Slots from elsewhere must stay in range.
        for slot in &mut this.slots.0 {
            *slot = (*slot).min(Slots::NONE);
        }
        this
    }
}

/// Length of [`ModulationValues::to_words`].
pub const MODULATION_WORDS: usize = MAX_ROUTES + 1 + TARGETS.div_ceil(4);

impl Offsets<'_> {
    /// These offsets, kept beyond the block.
    pub(crate) fn snapshot(&self) -> ModulationValues {
        ModulationValues {
            values: self.values,
            slots: *self.slots,
        }
    }
}

/// Outputs of all sources for one voice at one point in time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SourceValues {
    pub(crate) lfo: [f32; MAX_LFOS],
    pub(crate) envelope: [f32; MAX_ENVELOPES],
    pub(crate) velocity: f32,
    pub(crate) aftertouch: f32,
    pub(crate) mod_wheel: f32,
    pub(crate) pitch_bend: f32,
    pub(crate) key: f32,
    pub(crate) random: f32,
}

impl SourceValues {
    pub(crate) const ZERO: Self = Self {
        lfo: [0.0; MAX_LFOS],
        envelope: [0.0; MAX_ENVELOPES],
        velocity: 0.0,
        aftertouch: 0.0,
        mod_wheel: 0.0,
        pitch_bend: 0.0,
        key: 0.0,
        random: 0.0,
    };

    #[inline]
    fn value(&self, source: ModSource) -> f32 {
        match source {
            ModSource::None => 0.0,
            ModSource::Lfo(i) => self.lfo.get(i).copied().unwrap_or(0.0),
            ModSource::Envelope(i) => self.envelope.get(i).copied().unwrap_or(0.0),
            ModSource::Velocity => self.velocity,
            ModSource::Aftertouch => self.aftertouch,
            ModSource::ModWheel => self.mod_wheel,
            ModSource::PitchBend => self.pitch_bend,
            ModSource::KeyTrack => self.key,
            ModSource::Random => self.random,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(source: ModSource, target: ModTarget, depth: f32, via: ModSource) -> ModRoute {
        ModRoute {
            source,
            target,
            depth,
            via,
        }
    }

    #[test]
    fn routes_add_up_per_target() {
        let mut values = SourceValues::ZERO;
        values.lfo[2] = -0.5;
        values.envelope[1] = 0.25;
        values.velocity = 0.5;
        let routes = [
            route(
                ModSource::Lfo(2),
                ModTarget::OpPitch(3),
                12.0,
                ModSource::None,
            ),
            route(
                ModSource::Envelope(1),
                ModTarget::OpPitch(3),
                8.0,
                ModSource::None,
            ),
            route(
                ModSource::Velocity,
                ModTarget::Cutoff(0),
                24.0,
                ModSource::None,
            ),
            route(ModSource::Velocity, ModTarget::None, 1.0, ModSource::None),
        ];
        let offsets = Offsets::new(&routes, &values);
        assert_eq!(offsets.get(ModTarget::OpPitch(3)), -6.0 + 2.0);
        assert_eq!(offsets.get(ModTarget::Cutoff(0)), 12.0);
        assert_eq!(offsets.get(ModTarget::OpPitch(2)), 0.0);
        assert_eq!(offsets.get(ModTarget::None), 0.0);
    }

    #[test]
    fn via_scales_the_depth() {
        let mut values = SourceValues::ZERO;
        values.lfo[0] = 1.0;
        values.velocity = 0.25;
        let routes = [route(
            ModSource::Lfo(0),
            ModTarget::Tune,
            4.0,
            ModSource::Velocity,
        )];
        assert_eq!(Offsets::new(&routes, &values).get(ModTarget::Tune), 1.0);
    }

    #[test]
    fn targets_have_distinct_indices() {
        let mut all = vec![ModTarget::Volume, ModTarget::Tune, ModTarget::Balance];
        for i in 0..OPERATORS {
            all.extend([
                ModTarget::OpPitch(i),
                ModTarget::OpLevel(i),
                ModTarget::OpFeedback(i),
                ModTarget::OpPan(i),
                ModTarget::OpFold(i),
            ]);
        }
        for f in 0..FILTERS {
            all.extend([
                ModTarget::Cutoff(f),
                ModTarget::Resonance(f),
                ModTarget::Drive(f),
            ]);
        }
        for l in 0..MAX_LFOS {
            all.extend([
                ModTarget::LfoAmount(l),
                ModTarget::LfoRate(l),
                ModTarget::LfoFade(l),
            ]);
        }
        all.push(ModTarget::Glide);
        for i in 0..OPERATORS {
            all.push(ModTarget::OpRatio(i));
            for s in 0..ENVELOPE_STAGES {
                all.push(ModTarget::OpEnvelope(i, s));
            }
            for j in (0..OPERATORS).filter(|&j| j != i) {
                all.push(ModTarget::OpDepth(i, j));
            }
        }
        for e in 0..MAX_ENVELOPES {
            for s in 0..ENVELOPE_STAGES {
                all.push(ModTarget::ModEnvelope(e, s));
            }
        }
        let mut indices: Vec<usize> = all.iter().map(|t| t.index().unwrap()).collect();
        indices.sort();
        assert_eq!(indices, (0..TARGETS).collect::<Vec<_>>());
        assert_eq!(ModTarget::OpLevel(OPERATORS).index(), None);
        assert_eq!(ModTarget::LfoRate(MAX_LFOS).index(), None);
        assert_eq!(
            ModTarget::OpDepth(2, 2).index(),
            None,
            "feedback is OpFeedback"
        );
        assert_eq!(ModTarget::OpEnvelope(0, ENVELOPE_STAGES).index(), None);
        assert_eq!(ModTarget::ModEnvelope(MAX_ENVELOPES, 0).index(), None);
    }

    #[test]
    fn usage_counts_sources_and_vias() {
        let routes = [
            route(
                ModSource::Lfo(3),
                ModTarget::OpLevel(0),
                0.5,
                ModSource::Envelope(2),
            ),
            route(
                ModSource::Envelope(5),
                ModTarget::LfoAmount(3),
                0.5,
                ModSource::None,
            ),
            route(
                ModSource::Envelope(6),
                ModTarget::LfoRate(1),
                0.5,
                ModSource::None,
            ),
            route(
                ModSource::Lfo(4),
                ModTarget::OpLevel(1),
                0.0,
                ModSource::None,
            ),
        ];
        let (params, by_lfos) = usage(&routes, 1 << 3);
        assert_eq!(
            params,
            Usage {
                lfos: 1 << 3,
                envelopes: 1 << 2
            }
        );
        // LFO 1 is not in use: its rate route does not count.
        assert_eq!(
            by_lfos,
            Usage {
                lfos: 0,
                envelopes: 1 << 5
            }
        );
        assert!(
            ModSlot {
                source: ModSource::Velocity,
                depth: 1.0
            }
            .usage()
            .is_empty(),
            "velocity needs no computation"
        );
    }

    #[test]
    fn sanitized_drops_missing_sources_and_lfo_loops() {
        let counts = ModCounts {
            lfos: 4,
            envelopes: 1,
        };
        let r = route(
            ModSource::Lfo(3),
            ModTarget::OpPitch(0),
            100.0,
            ModSource::Envelope(1),
        )
        .sanitized(counts);
        assert_eq!(
            r,
            route(
                ModSource::Lfo(3),
                ModTarget::OpPitch(0),
                MAX_PITCH_DEPTH,
                ModSource::None
            )
        );
        let r = route(
            ModSource::Lfo(4),
            ModTarget::OpLevel(0),
            f32::NAN,
            ModSource::None,
        )
        .sanitized(counts);
        assert_eq!((r.source, r.depth), (ModSource::None, 0.0));
        // LFOs never modulate LFOs, neither as source nor as via.
        let r = route(
            ModSource::Lfo(0),
            ModTarget::LfoRate(1),
            1.0,
            ModSource::Lfo(2),
        )
        .sanitized(counts);
        assert_eq!((r.source, r.via), (ModSource::None, ModSource::None));
        let r = route(
            ModSource::Envelope(0),
            ModTarget::LfoRate(1),
            100.0,
            ModSource::None,
        )
        .sanitized(counts);
        assert_eq!(r.depth, MAX_RATE_DEPTH);
        // Targets that do not exist.
        let r = route(
            ModSource::Velocity,
            ModTarget::LfoAmount(5),
            1.0,
            ModSource::None,
        )
        .sanitized(counts);
        assert_eq!(r.target, ModTarget::None);
        let r = route(
            ModSource::Velocity,
            ModTarget::OpFold(OPERATORS),
            1.0,
            ModSource::None,
        )
        .sanitized(counts);
        assert_eq!(r.target, ModTarget::None);
        assert!(!r.is_active());
        // Envelope stages: no modulators as sources; only envelopes that exist.
        let stage = ModTarget::ModEnvelope(0, 1);
        let r = route(ModSource::Envelope(0), stage, 0.5, ModSource::Lfo(0)).sanitized(counts);
        assert_eq!((r.source, r.via), (ModSource::None, ModSource::None));
        let r = route(
            ModSource::KeyTrack,
            ModTarget::ModEnvelope(1, 1),
            0.5,
            ModSource::None,
        )
        .sanitized(counts);
        assert_eq!(r.target, ModTarget::None);
        let r = route(
            ModSource::Lfo(0),
            ModTarget::LfoFade(1),
            0.5,
            ModSource::None,
        )
        .sanitized(counts);
        assert_eq!(r.source, ModSource::None);
        let r = route(
            ModSource::Lfo(0),
            ModTarget::OpEnvelope(0, 1),
            0.5,
            ModSource::None,
        )
        .sanitized(counts);
        assert_eq!(
            r.source,
            ModSource::Lfo(0),
            "operator envelopes run after the LFOs"
        );
    }

    #[test]
    fn modulation_values_survive_words() {
        let routes = [route(
            ModSource::Velocity,
            ModTarget::OpDepth(3, 1),
            0.5,
            ModSource::None,
        )];
        let values = SourceValues {
            velocity: 0.5,
            ..SourceValues::ZERO
        };
        let live = Offsets::new(&routes, &values).snapshot();
        let back = ModulationValues::from_words(&live.to_words());
        assert_eq!(back, live);
        assert_eq!(back.get(ModTarget::OpDepth(3, 1)), 0.25);
        assert_eq!(back.get(ModTarget::OpLevel(0)), 0.0);
    }
}
