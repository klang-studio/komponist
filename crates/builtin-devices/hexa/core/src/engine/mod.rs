//! Polyphonic FM engine: [`OPERATORS`] operators per voice with five
//! modulation modes, waveforms, key scaling, voice filters, LFOs and
//! modulation envelopes, mono or stereo, optionally 2× oversampled.
//!
//! An engine renders voices; which key plays on which voice decides
//! [`crate::Synth`], the only way to play it, through [`Voices`].
//!
//! Voice state is stored as structure of arrays with one lane per voice.
//! This module holds the engine's state and the voice control (start,
//! release); `render` the per-block rendering, `routing` the patch's
//! modulation routing as bit masks.

mod render;
mod routing;
#[cfg(test)]
mod tests;

use crate::allocator::Allocation;
use crate::controls::{self, Controls};
use crate::filter::{FILTERS, FilterSmooth, FilterState};
use crate::lfo::{LFO_POSITIONS, LfoPositions, Lfos};
use crate::midi::DATA_MAX;
use crate::midi::MidiMessage;
use crate::mod_envelope::{MAX_ENVELOPES, ModEnvelopes};
use crate::modulation::{Modulated, ModulationValues, Routes, Usage};
use crate::operator::{OPERATORS, Operator};
use crate::oversampling::HalfBand;
use crate::simd::LANES;
use crate::voicing::{Layer, Voices, VoicingParams};
use crate::{BLOCK_SIZE, MAX_VOICES, Patch};
use routing::Routing;

/// Smoothing time constant for patch parameter changes.
const SMOOTHING_SECONDS: f32 = 0.01;

pub struct Engine {
    sample_rate: f32,
    patch: Patch,
    /// Derived from `patch` whenever it changes.
    routing: Routing,
    usage: Usage,
    modulated: Modulated,
    routes: Routes,
    /// Smoothed master volume, tuning and balance, chasing the patch values.
    volume: f32,
    tune: f32,
    balance: f32,
    /// Smoothed unison spread, chasing the patch's (0 without unison).
    spread: f32,

    lfos: Lfos,
    mod_envelopes: ModEnvelopes,
    /// Mod wheel, pitch wheel and channel pressure (modulation sources).
    controls: Controls,
    velocity: [f32; MAX_VOICES],
    /// Per voice: the pressure of its key (polyphonic aftertouch), its
    /// random value and how many notes it started (for that value).
    pressure: [f32; MAX_VOICES],
    random: [f32; MAX_VOICES],
    starts: [u32; MAX_VOICES],
    /// Per voice: its modulation at the end of its last block (displays).
    live: [ModulationValues; MAX_VOICES],
    /// Per voice: the played note, the current pitch gliding towards it (in
    /// semitones) and the distance the glide covers in the glide time.
    target: [f32; MAX_VOICES],
    pitch: [f32; MAX_VOICES],
    glide_span: [f32; MAX_VOICES],
    /// Per voice: its place in the key's unison group.
    layer: [Layer; MAX_VOICES],
    operators: [Operator; OPERATORS],

    /// The output of the last block. Mono (`mix` only) while every heard
    /// operator and the master balance are centred and unmodulated, else
    /// stereo (`mix` left, `mix_r` right); see `stereo`.
    mix: [f32; BLOCK_SIZE],
    mix_r: [f32; BLOCK_SIZE],
    stereo: bool,
    /// Filter state per filter and voice; the right channel's is only used
    /// in stereo.
    filter_state: [[FilterState; MAX_VOICES]; FILTERS],
    filter_state_r: [[FilterState; MAX_VOICES]; FILTERS],
    /// Smoothed filter parameters, chasing the patch values.
    filter_smooth: [FilterSmooth; FILTERS],
    /// Voices (bit mask) whose filters still ring after their envelopes
    /// ended; they are rendered until the filters are silent, so resonance
    /// decays instead of being cut off.
    ringing: u32,
    /// Operators render at `oversampling` × the sample rate (1 or 2); at 2,
    /// into `mix_oversampled`, which `decimator` reduces into `mix`.
    oversampling: usize,
    mix_oversampled: [f32; 2 * BLOCK_SIZE],
    mix_oversampled_r: [f32; 2 * BLOCK_SIZE],
    decimator: HalfBand,
    decimator_r: HalfBand,
    /// One voice's output before filtering and mixing; fields rather than
    /// locals, so nothing is cleared per voice (every sample is written).
    voice: [f32; 2 * BLOCK_SIZE],
    voice_r: [f32; 2 * BLOCK_SIZE],
    /// The operator output of a group of voices, one per SIMD lane, before
    /// it is copied into `voice` voice by voice.
    lanes: [[f32; LANES]; 2 * BLOCK_SIZE],
    lanes_r: [[f32; LANES]; 2 * BLOCK_SIZE],
}

/// Number of envelopes in [`EnvelopePositions`]: the operators' first, then
/// the modulation envelopes.
pub const ENVELOPE_POSITIONS: usize = OPERATORS + MAX_ENVELOPES;

/// Where every envelope of the newest sounding voice is, see
/// [`crate::envelope::Envelope::position`]; -1 for idle or unused ones.
pub type EnvelopePositions = [f32; ENVELOPE_POSITIONS];

/// Phase step between unison voices: the golden ratio of a cycle, so any
/// number of voices spread evenly.
const UNISON_PHASE: u32 = 0x9e37_79b9;

/// The voices of a single engine; [`crate::Synth`] calls these on the
/// engine owning each voice.
impl Voices for Engine {
    fn is_sounding(&self, voice: usize) -> bool {
        Engine::is_sounding(self, voice)
    }

    fn start(
        &mut self,
        allocation: Allocation,
        note: u8,
        velocity: u8,
        glide_from: Option<f32>,
        layer: Layer,
    ) {
        let (v, fresh) = match allocation {
            Allocation::Retrigger(v) => (v, false),
            Allocation::Fresh(v) | Allocation::Steal(v) => {
                // Stealing cuts the old note hard; a short fade-out would avoid the click.
                // Unison voices start at different phases, else they sound as
                // one until the detune pulls them apart (a supersaw does the
                // same); the first keeps phase 0.
                let phase = UNISON_PHASE.wrapping_mul(layer.index as u32);
                self.operators
                    .iter_mut()
                    .for_each(|op| op.reset_voice(v, phase));
                self.reset_filters(v);
                (v, true)
            }
        };
        self.layer[v] = layer;
        self.target[v] = note as f32;
        match glide_from {
            Some(from) if self.patch.portamento > 0.0 => {
                self.pitch[v] = from;
                self.glide_span[v] = (note as f32 - from).abs();
            }
            _ => self.pitch[v] = note as f32,
        }
        // Linear velocity → amplitude mapping, 127 = full level.
        self.velocity[v] = velocity.min(DATA_MAX) as f32 / DATA_MAX as f32;
        self.pressure[v] = 0.0;
        self.starts[v] = self.starts[v].wrapping_add(1);
        self.random[v] = controls::note_random(v, self.starts[v]);
        self.lfos.note_on(v, self.patch.lfos.as_slice());
        self.mod_envelopes.note_on(v, fresh);
        self.operators
            .iter_mut()
            .for_each(|op| op.envelope[v].gate_on());
    }

    fn legato(&mut self, voice: usize, note: u8, glide: bool) {
        self.target[voice] = note as f32;
        if glide && self.patch.portamento > 0.0 {
            self.glide_span[voice] = (note as f32 - self.pitch[voice]).abs();
        } else {
            self.pitch[voice] = note as f32;
        }
    }

    fn release(&mut self, voices: u32) {
        debug_assert!(voices >> MAX_VOICES == 0, "voice mask {voices:#b}");
        for v in voice_bits(voices) {
            self.operators
                .iter_mut()
                .for_each(|op| op.envelope[v].gate_off());
            self.mod_envelopes.note_off(v);
        }
    }

    fn silence(&mut self, voices: u32) {
        for v in voice_bits(voices) {
            self.operators
                .iter_mut()
                .for_each(|op| op.reset_voice(v, 0));
            self.mod_envelopes.reset(v);
            self.reset_filters(v);
        }
    }

    fn pitch(&self, voice: usize) -> f32 {
        self.pitch[voice]
    }
}

/// Indices of the set bits of a voice mask below [`MAX_VOICES`], lowest first.
fn voice_bits(mut voices: u32) -> impl Iterator<Item = usize> {
    voices &= u32::MAX >> (32 - MAX_VOICES);
    core::iter::from_fn(move || {
        (voices != 0).then(|| {
            let v = voices.trailing_zeros() as usize;
            voices &= voices - 1;
            v
        })
    })
}

/// Clamps every sample to -1..1.
pub fn clamp(samples: &mut [f32]) {
    samples.iter_mut().for_each(|x| *x = x.clamp(-1.0, 1.0));
}

impl Engine {
    pub(crate) fn with_patch(sample_rate: f32, patch: Patch) -> Self {
        let patch = patch.sanitized();
        Self {
            sample_rate,
            patch,
            routing: Routing::new(&patch),
            usage: patch.usage(),
            modulated: Modulated::new(patch.modulation.as_slice()),
            routes: Routes::new(patch.modulation.as_slice()),
            volume: patch.volume,
            tune: patch.tune,
            balance: patch.balance,
            spread: patch.spread(),
            lfos: Lfos::new(),
            mod_envelopes: ModEnvelopes::new(),
            controls: Controls::new(),
            velocity: [0.0; MAX_VOICES],
            pressure: [0.0; MAX_VOICES],
            random: [0.0; MAX_VOICES],
            starts: [0; MAX_VOICES],
            live: [ModulationValues::NONE; MAX_VOICES],
            target: [0.0; MAX_VOICES],
            pitch: [0.0; MAX_VOICES],
            glide_span: [0.0; MAX_VOICES],
            layer: [Layer::SINGLE; MAX_VOICES],
            operators: core::array::from_fn(|i| Operator::new(i, &patch.operators[i])),
            mix: [0.0; BLOCK_SIZE],
            mix_r: [0.0; BLOCK_SIZE],
            stereo: false,
            filter_state: [[FilterState::default(); MAX_VOICES]; FILTERS],
            filter_state_r: [[FilterState::default(); MAX_VOICES]; FILTERS],
            filter_smooth: patch.filters.map(|f| FilterSmooth::new(&f)),
            ringing: 0,
            oversampling: 2,
            mix_oversampled: [0.0; 2 * BLOCK_SIZE],
            mix_oversampled_r: [0.0; 2 * BLOCK_SIZE],
            decimator: HalfBand::new(),
            decimator_r: HalfBand::new(),
            voice: [0.0; 2 * BLOCK_SIZE],
            voice_r: [0.0; 2 * BLOCK_SIZE],
            lanes: [[0.0; LANES]; 2 * BLOCK_SIZE],
            lanes_r: [[0.0; LANES]; 2 * BLOCK_SIZE],
        }
    }

    /// Renders the operators at `factor` × the sample rate: 2 (the default)
    /// keeps the harmonics of strong FM and saw/square waves from aliasing
    /// up to twice the output's Nyquist frequency, at about twice the CPU
    /// cost; 1 renders directly. Other values are clamped to that range.
    /// At 2 the output is delayed by [`crate::oversampling::DELAY`] samples.
    pub(crate) fn set_oversampling(&mut self, factor: usize) {
        let factor = factor.clamp(1, 2);
        if factor != self.oversampling {
            self.oversampling = factor;
            self.decimator.reset();
            self.decimator_r.reset();
        }
    }

    pub(crate) fn oversampling(&self) -> usize {
        self.oversampling
    }

    pub(crate) fn patch(&self) -> &Patch {
        &self.patch
    }

    pub(crate) fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Changes the patch. Ratios, levels and volume glide to the new values.
    /// Out-of-range values are clamped, see [`Patch::sanitized`].
    pub(crate) fn set_patch(&mut self, patch: Patch) {
        let patch = patch.sanitized();
        // Filters that change their kind or slope start from silence instead
        // of state left over from another filter (or from long ago).
        for f in 0..FILTERS {
            let (old, new) = (&self.patch.filters[f], &patch.filters[f]);
            if old.kind != new.kind || old.slope != new.slope {
                self.filter_state[f] = [FilterState::default(); MAX_VOICES];
                self.filter_state_r[f] = [FilterState::default(); MAX_VOICES];
            }
        }
        // LFOs added while notes sound start for them now.
        let lfos = patch.lfos.as_slice();
        let added = self.patch.lfos.len();
        if lfos.len() > added {
            let sounding: [bool; MAX_VOICES] = core::array::from_fn(|v| self.is_sounding(v));
            for v in (0..MAX_VOICES).filter(|&v| sounding[v]) {
                for (l, params) in lfos.iter().enumerate().skip(added) {
                    self.lfos.start(l, v, params);
                }
            }
        }
        self.patch = patch;
        self.routing = Routing::new(&patch);
        self.usage = patch.usage();
        self.modulated = Modulated::new(patch.modulation.as_slice());
        self.routes = Routes::new(patch.modulation.as_slice());
    }

    /// Takes the MIDI controllers that modulate (see [`Controls`]) and
    /// polyphonic aftertouch for the voices playing its key; ignores the rest.
    pub(crate) fn control(&mut self, msg: &MidiMessage) {
        self.controls.handle(msg);
        if let MidiMessage::PolyPressure { note, pressure, .. } = *msg {
            for v in 0..MAX_VOICES {
                if self.target[v] == note as f32 && self.is_sounding(v) {
                    self.pressure[v] = controls::key_pressure(pressure);
                }
            }
        }
    }

    /// A voice sounds as long as the envelope of any heard operator is not idle.
    pub(crate) fn is_sounding(&self, voice: usize) -> bool {
        is_sounding(&self.operators, self.routing.outputs, voice)
    }

    /// Number of voices currently producing sound.
    pub fn active_voices(&self) -> usize {
        (0..MAX_VOICES).filter(|&v| self.is_sounding(v)).count()
    }

    /// The patch's voice mode and portamento.
    pub(crate) fn voicing_params(&self) -> VoicingParams {
        VoicingParams {
            mode: self.patch.voice_mode,
            portamento: self.patch.portamento,
            unison: self.patch.unison as usize,
        }
    }

    fn reset_filters(&mut self, voice: usize) {
        self.ringing &= !(1 << voice);
        self.filter_state
            .iter_mut()
            .for_each(|states| states[voice] = FilterState::default());
        self.filter_state_r
            .iter_mut()
            .for_each(|states| states[voice] = FilterState::default());
    }

    /// Renders into both channels, without clamping to -1..1, so the
    /// outputs of several engines can be summed (see [`crate::Synth`]).
    /// Events take effect at the start of the call. Only the common length
    /// of the two slices is written.
    pub fn render_unclamped(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = out_l.len().min(out_r.len());
        for (l, r) in out_l[..frames]
            .chunks_mut(BLOCK_SIZE)
            .zip(out_r[..frames].chunks_mut(BLOCK_SIZE))
        {
            self.render_block(l.len());
            let right = if self.stereo { &self.mix_r } else { &self.mix };
            l.copy_from_slice(&self.mix[..l.len()]);
            r.copy_from_slice(&right[..r.len()]);
        }
    }

    /// Modulation of `voice` at the end of its last block.
    pub(crate) fn modulation_of(&self, voice: usize) -> ModulationValues {
        self.live[voice]
    }

    /// Envelope positions of `voice`.
    pub(crate) fn envelope_positions_of(&self, voice: usize) -> EnvelopePositions {
        let used = self.usage.envelopes;
        let envelopes = self.patch.envelopes.as_slice();
        core::array::from_fn(|k| {
            if k < OPERATORS {
                self.operators[k].envelope[voice].position(&self.patch.operators[k].envelope)
            } else {
                let e = k - OPERATORS;
                match envelopes.get(e) {
                    Some(params) if used & (1 << e) != 0 => {
                        self.mod_envelopes.get(e, voice).position(params)
                    }
                    _ => -1.0,
                }
            }
        })
    }

    /// LFO positions of `voice`.
    pub(crate) fn lfo_positions_of(&self, voice: usize) -> LfoPositions {
        let used = self.usage.lfos;
        let count = self.patch.lfos.len();
        let mut positions = [0.0; LFO_POSITIONS];
        for (l, p) in positions.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            *p = if l < count && used & (1 << l) != 0 {
                self.lfos.position(l, voice)
            } else {
                [-1.0, 0.0, 0.0]
            };
        }
        positions
    }

    /// Seeds the random generator of sample-and-hold LFOs, so engines that
    /// share the voices (see [`crate::Synth`]) do not repeat each other.
    pub(crate) fn seed(&mut self, seed: u32) {
        self.lfos.seed(seed);
    }
}

/// Whether `voice` still sounds: the envelope of any heard operator is not idle.
fn is_sounding(operators: &[Operator; OPERATORS], outputs: u8, voice: usize) -> bool {
    (0..OPERATORS).any(|i| outputs & (1 << i) != 0 && !operators[i].envelope[voice].is_idle())
}
