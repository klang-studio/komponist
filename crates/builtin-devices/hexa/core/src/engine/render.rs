//! Rendering one block: control-rate smoothing, then every sounding voice
//! (operators, filters, mix), then decimation.
//!
//! The operators of up to [`LANES`] voices render together, one voice per
//! SIMD lane (NEON on the Pi): the routing is the same for all voices, so
//! the lanes never diverge. Everything else runs per voice.

use super::routing::{Routing, bits};
use super::{Engine, SMOOTHING_SECONDS, is_sounding};
use crate::controls::ControlValues;
use crate::envelope::EnvelopeCoefs;
use crate::filter::{Coefs, FILTERS, FilterKind, FilterRun, FilterSmooth, damping, warp};
use crate::lfo::MAX_LFOS;
use crate::mod_envelope::{self, MAX_ENVELOPES};
use crate::modulation::{ModTarget, Offsets, Slots, SourceValues, Usage, time_offset};
use crate::operator::{
    MAX_FEEDBACK, MAX_FOLD, MAX_MODULATION, MAX_SYNC_OCTAVES, OPERATORS, OperatorKind, PitchMode,
};
use crate::simd::{F32x4, LANES, U32x4};
use crate::smooth::{self, chase};
use crate::voicing::MAX_PORTAMENTO;
use crate::waveform::{self, Waveform};
use crate::{MAX_VOICES, phase, pitch, sine::sine4};

/// Pulse width modulation as a phase offset for the carrier: within each
/// modulator cycle (`mod_phase`), the first half of the carrier's waveform
/// plays in `width` = (1 − depth) / 2 of the time and the second half in the
/// rest. `cycles` is the number of carrier cycles per modulator cycle, which
/// keeps the warp phase-continuous for any ratio.
#[inline(always)]
fn pulse_width_offset(mod_phase: U32x4, depth: F32x4, cycles: F32x4) -> U32x4 {
    let s = F32x4::splat;
    let t = mod_phase.to_unit();
    let width = (s(0.5) * (s(1.0) - depth)).clamp(1e-4, 0.5);
    let warped = t.lt(width).select(
        s(0.5) * t / width,
        s(0.5) + s(0.5) * (t - width) / (s(1.0) - width),
    );
    ((warped - t) * cycles * s(phase::PERIOD_F32)).to_u32_wrapping()
}

/// The wavefolder on an operator's waveform `x`, for a fold amount `a`
/// (0..=[`MAX_FOLD`]): up to 1 it fades from `x` to sin(π/2 · x), beyond
/// that the gain into the sine rises to `a`, folding the wave back at ±1.
/// Exactly `x` at 0, and no jump anywhere, so the amount can glide.
#[inline(always)]
fn fold(x: F32x4, a: F32x4) -> F32x4 {
    let s = F32x4::splat;
    let below = a.lt(s(1.0));
    let (gain, wet) = (below.select(s(1.0), a), below.select(a, s(1.0)));
    // sin(π/2 · u) is the sine of u quarter periods.
    let folded = sine4((x * gain * s(0.25 * phase::PERIOD_F32)).to_u32_wrapping());
    x + (folded - x) * wet
}

/// Highest and lowest cutoff of a filter operator, as fractions of the rate
/// (the voice filter's limits, see [`warp`]).
const MAX_CUTOFF: f32 = 0.45;
const MIN_CUTOFF_HZ: f32 = 10.0;

/// tan(π · f) for cutoffs `f` (fractions of the rate) up to [`MAX_CUTOFF`]:
/// a Padé approximation, within 3.5 % there and far closer below, cheap
/// enough for filter FM at audio rate.
#[inline(always)]
fn tan_pi(f: F32x4) -> F32x4 {
    let s = F32x4::splat;
    let x = f * s(core::f32::consts::PI);
    let x2 = x * x;
    x * (s(15.0) - x2) / (s(15.0) - s(6.0) * x2)
}

/// Per-block values shared by all voices.
struct Block {
    n: usize,
    ns: usize,
    rate: f32,
    /// 1 / `ns`, for ramps across the block.
    inv_ns: f32,
    block_seconds: f32,
    volume0: f32,
    tune0: f32,
    balance0: f32,
    spread0: f32,
    filters_from: [FilterSmooth; FILTERS],
    filters_to: [FilterSmooth; FILTERS],
    levels: [(f32, f32); OPERATORS],
    pans: [(f32, f32); OPERATORS],
    folds: [(f32, f32); OPERATORS],
    /// Bit `i`: operator `i` folds in this block (for some voice).
    folding: u8,
    /// Bit `i`: operator `i` is a filter; its kind and damping (k = 1/Q).
    filter_ops: u8,
    kinds: [OperatorKind; OPERATORS],
    dampings: [f32; OPERATORS],
    /// `depths[i][j]`: smoothed depth of source `j` on target `i`.
    depths: [[f32; OPERATORS]; OPERATORS],
    coefs: [EnvelopeCoefs; OPERATORS],
    waveforms: [Waveform; OPERATORS],
    routing: Routing,
    output_gain: f32,
    usage: Usage,
    envelope_coefs: [EnvelopeCoefs; MAX_ENVELOPES],
    /// The MIDI controllers at the start and the end of the block.
    controls: (ControlValues, ControlValues),
    /// Operators and modulation envelopes (bit masks) with modulated
    /// stages, and whether any modulation depth is modulated: these need
    /// per-voice values.
    op_envelopes: u8,
    mod_envelopes: u8,
    depths_modulated: bool,
    /// Operators (bit mask) with a modulated frequency factor.
    ratios: u8,
    /// The patch's modulation slots (see [`Slots`]); the voices' offsets
    /// read through them.
    slots: Slots,
    /// `depths` in every lane, for groups without modulated depths.
    lane_depths: Depths,
}

use klang_dsp::pan::balance as balance_gains;

impl Engine {
    /// Renders `n <= BLOCK_SIZE` samples into `self.mix` (and `self.mix_r`
    /// in stereo, see `self.stereo`).
    pub(super) fn render_block(&mut self, n: usize) {
        let block_seconds = n as f32 / self.sample_rate;
        // Operators run at the oversampled rate: `ns` samples for this block.
        let rate = self.sample_rate * self.oversampling as f32;
        let ns = n * self.oversampling;
        let inv_ns = 1.0 / ns as f32;

        // Control rate: smooth patch parameters and derive envelope coefficients.
        let smooth = smooth::amount(block_seconds, SMOOTHING_SECONDS);
        let volume0 = self.volume;
        chase(&mut self.volume, self.patch.volume, smooth);
        let tune0 = self.tune;
        chase(&mut self.tune, self.patch.tune, smooth);
        let balance0 = self.balance;
        chase(&mut self.balance, self.patch.balance, smooth);
        let spread0 = self.spread;
        chase(&mut self.spread, self.patch.spread(), smooth);
        let filters_from = self.filter_smooth;
        for (smoothed, params) in self.filter_smooth.iter_mut().zip(&self.patch.filters) {
            smoothed.chase(params, smooth);
        }
        let filters_to = self.filter_smooth;
        let controls = self.controls.advance(smooth);
        let mut levels = [(0.0, 0.0); OPERATORS];
        let mut pans = [(0.0, 0.0); OPERATORS];
        let mut folds = [(0.0, 0.0); OPERATORS];
        let mut folding = 0;
        for (i, op) in self.operators.iter_mut().enumerate() {
            let params = &self.patch.operators[i];
            [levels[i], pans[i], folds[i]] = op.smooth(params, smooth);
            if folds[i] != (0.0, 0.0) || self.modulated.has(ModTarget::OpFold(i)) {
                folding |= 1 << i;
            }
        }
        let kinds: [OperatorKind; OPERATORS] =
            core::array::from_fn(|i| self.patch.operators[i].kind);
        let filter_ops = (0..OPERATORS)
            .filter(|&i| kinds[i] != OperatorKind::Oscillator)
            .fold(0, |mask, i| mask | 1 << i);
        let (routing, usage) = (self.routing, self.usage);
        let depths: [[f32; OPERATORS]; OPERATORS] =
            core::array::from_fn(|i| core::array::from_fn(|j| self.operators[j].depths[i]));
        let block = Block {
            n,
            ns,
            rate,
            inv_ns,
            block_seconds,
            volume0,
            tune0,
            balance0,
            spread0,
            filters_from,
            filters_to,
            levels,
            pans,
            folds,
            folding,
            filter_ops,
            kinds,
            dampings: core::array::from_fn(|i| damping(self.operators[i].resonance)),
            depths,
            coefs: core::array::from_fn(|i| {
                EnvelopeCoefs::new(&self.patch.operators[i].envelope, block_seconds)
            }),
            waveforms: core::array::from_fn(|i| self.patch.operators[i].waveform),
            routing,
            // Keep the loudness independent of the number of heard operators.
            output_gain: 1.0 / routing.outputs.count_ones().max(1) as f32,
            usage,
            envelope_coefs: mod_envelope::coefs(
                &self.patch.envelopes,
                usage.envelopes,
                block_seconds,
            ),
            controls,
            op_envelopes: self.modulated.op_envelopes(),
            mod_envelopes: self.modulated.mod_envelopes(),
            depths_modulated: self.modulated.depths(),
            ratios: self.modulated.ratios(),
            slots: *self.routes.slots(),
            lane_depths: Depths::splat(&depths),
        };
        self.lfos
            .advance_global(self.patch.lfos.as_slice(), block_seconds);

        // Stereo only while anything heard can be off-centre (smoothing
        // included); mono renders as fast as before.
        let heard_panned = (0..OPERATORS).any(|i| {
            let panned =
                self.patch.operators[i].pan != 0.0 || self.modulated.has(ModTarget::OpPan(i));
            routing.outputs & (1 << i) != 0 && (panned || pans[i] != (0.0, 0.0))
        });
        let stereo = heard_panned
            || balance0 != 0.0
            || self.balance != 0.0
            || self.modulated.has(ModTarget::Balance)
            || spread0 != 0.0
            || self.spread != 0.0;
        if stereo && !self.stereo {
            // Continue the right channel from the mono state.
            self.filter_state_r = self.filter_state;
            self.decimator_r = self.decimator.clone();
        }
        self.stereo = stereo;
        if stereo {
            self.render_voices::<true>(&block);
        } else {
            self.render_voices::<false>(&block);
        }
    }

    /// The voices of one block: mono into `mix`, or with `STEREO` into `mix`
    /// and `mix_r`. Generic, so the mono version carries no stereo code.
    fn render_voices<const STEREO: bool>(&mut self, block: &Block) {
        let (n, ns) = (block.n, block.ns);
        let len = if self.oversampling == 2 { ns } else { n };
        self.mix_buffer().0[..len].fill(0.0);
        if STEREO {
            self.mix_buffer().1[..len].fill(0.0);
        }

        // Sounding voices in groups of `LANES`, in voice order.
        let mut group = [0; LANES];
        let mut count = 0;
        for v in 0..MAX_VOICES {
            if !is_sounding(&self.operators, block.routing.outputs, v)
                && self.ringing & (1 << v) == 0
            {
                continue;
            }
            group[count] = v;
            count += 1;
            if count == LANES {
                self.render_group::<STEREO>(&group, block, len);
                count = 0;
            }
        }
        if count > 0 {
            self.render_group::<STEREO>(&group[..count], block, len);
        }

        if self.oversampling == 2 {
            self.decimator
                .decimate(&self.mix_oversampled[..ns], &mut self.mix[..n]);
            if STEREO {
                self.decimator_r
                    .decimate(&self.mix_oversampled_r[..ns], &mut self.mix_r[..n]);
            }
        }
    }

    /// Up to [`LANES`] voices: their operators render together, one voice
    /// per lane (unused lanes render silence); set-up, filters and mixing
    /// run per voice, in the same order as voice by voice.
    fn render_group<const STEREO: bool>(&mut self, voices: &[usize], block: &Block, len: usize) {
        let mut lanes: [OpLanes; LANES] = Default::default();
        let mut pre = [0.0; LANES];
        let mut amps = [Ramp::default(); LANES];
        // The voices' filters for this block, set up while their offsets are at hand.
        let mut filters: [Option<[FilterRun; FILTERS]>; LANES] = [const { None }; LANES];
        let mut depths = [block.depths; LANES];
        for (k, &v) in voices.iter().enumerate() {
            // This block plays the pitch at its start.
            let note = self.pitch[v];
            let (o0, o1) = self.voice_offsets(v, note, block);
            let glide = if self.modulated.glide() {
                time_offset(
                    self.patch.portamento,
                    o0.get(ModTarget::Glide),
                    MAX_PORTAMENTO,
                )
            } else {
                self.patch.portamento
            };
            self.advance_glide(v, block.block_seconds, glide);
            if block.depths_modulated {
                for (to, row) in depths[k].iter_mut().enumerate() {
                    for (from, depth) in row.iter_mut().enumerate() {
                        *depth = (*depth + o0.get(ModTarget::OpDepth(from, to))).clamp(0.0, 1.0);
                    }
                }
            }
            self.live[v] = o1.snapshot();
            amps[k] = self.voice_amp::<STEREO>(v, block, (&o0, &o1));
            lanes[k] = self.load_operators::<STEREO>(v, note, block, &depths[k], (&o0, &o1));
            // Voice signal before the filters; the volume (with its
            // modulation) is applied after them, ramped across the block.
            // Unison voices of a key add up: 1/√n keeps their sum about as
            // loud as one voice.
            let count = self.layer[v].count;
            let unison_gain = if count > 1 {
                1.0 / libm::sqrtf(count as f32)
            } else {
                1.0
            };
            pre[k] = self.velocity[v] * block.output_gain * unison_gain;
            filters[k] = Some(core::array::from_fn(|f| {
                let (from, to) = (&block.filters_from[f], &block.filters_to[f]);
                FilterRun::new(
                    f,
                    &self.patch.filters[f],
                    (from, to),
                    (&o0, &o1),
                    note,
                    block.rate,
                    block.inv_ns,
                )
            }));
        }

        let mut ops = OpVectors::gather(&lanes);
        let gathered;
        let depths = if block.depths_modulated {
            gathered = Depths::gather(&depths);
            &gathered
        } else {
            &block.lane_depths
        };
        let pre = F32x4::from_array(pre);
        let (voice, voice_r) = (&mut self.lanes[..len], &mut self.lanes_r[..len]);
        if block.folding | block.filter_ops != 0 {
            render_operators::<STEREO, true>(&mut ops, block, depths, pre, voice, voice_r);
        } else {
            render_operators::<STEREO, false>(&mut ops, block, depths, pre, voice, voice_r);
        }
        ops.scatter(&mut lanes);

        for (k, &v) in voices.iter().enumerate() {
            lanes[k].store(&mut self.operators, v);
            for (x, lane) in self.voice[..len].iter_mut().zip(&self.lanes[..len]) {
                *x = lane[k];
            }
            if STEREO {
                for (x, lane) in self.voice_r[..len].iter_mut().zip(&self.lanes_r[..len]) {
                    *x = lane[k];
                }
            }
            if let Some(filters) = &mut filters[k] {
                self.filter_voice::<STEREO>(v, filters, len);
            }
            self.mix_voice::<STEREO>(amps[k], len);
        }
    }

    /// The buffers the voices are mixed into (left, right): at the
    /// oversampled rate or directly the output.
    fn mix_buffer(&mut self) -> (&mut [f32], &mut [f32]) {
        if self.oversampling == 2 {
            (&mut self.mix_oversampled, &mut self.mix_oversampled_r)
        } else {
            (&mut self.mix, &mut self.mix_r)
        }
    }

    /// Moves the voice's pitch along its glide, which covers its distance
    /// in `time` seconds (0 = at once).
    fn advance_glide(&mut self, v: usize, block_seconds: f32, time: f32) {
        let note = self.pitch[v];
        if note != self.target[v] {
            let step = if time > 0.0 {
                self.glide_span[v] / time * block_seconds
            } else {
                f32::INFINITY
            };
            let left = self.target[v] - note;
            self.pitch[v] = if left.abs() <= step {
                self.target[v]
            } else {
                note + step.copysign(left)
            };
        }
    }

    /// The modulation matrix's offsets for the voice (playing `note`) at the
    /// start and the end of the block; pitch and feedback use the start,
    /// levels and loudness ramp between both. In stages: envelopes, then
    /// LFOs (whose own modulation follows everything else), then everything.
    fn voice_offsets<'b>(
        &mut self,
        v: usize,
        note: f32,
        block: &'b Block,
    ) -> (Offsets<'b>, Offsets<'b>) {
        let routes = &self.routes;
        if routes.is_empty() {
            return (Offsets::ZERO, Offsets::ZERO);
        }
        // The sources that need no computation, at both ends of the block.
        let base = |c: &ControlValues| SourceValues {
            velocity: self.velocity[v],
            aftertouch: c.pressure.max(self.pressure[v]),
            mod_wheel: c.mod_wheel,
            pitch_bend: c.pitch_bend,
            key: ((note - 60.0) * (1.0 / 64.0)).clamp(-1.0, 1.0),
            random: self.random[v],
            ..SourceValues::ZERO
        };
        let (base0, base1) = (base(&block.controls.0), base(&block.controls.1));
        let usage = block.usage;
        if usage.is_empty() {
            return (
                routes.offsets(&block.slots, &base0),
                routes.offsets(&block.slots, &base1),
            );
        }
        let (env0, env1) = if usage.envelopes & block.mod_envelopes != 0 {
            // Stages moved per voice, by sources that are no modulators.
            let offsets = routes.envelope_offsets(&block.slots, &base0);
            let mut coefs = block.envelope_coefs;
            let params = self.patch.envelopes.as_slice();
            for e in (0..MAX_ENVELOPES)
                .filter(|&e| usage.envelopes & block.mod_envelopes & (1 << e) != 0)
            {
                coefs[e] =
                    coefs[e].modulated(&params[e], |s| offsets.get(ModTarget::ModEnvelope(e, s)));
            }
            self.mod_envelopes.advance_voice(v, &coefs, usage.envelopes)
        } else if usage.envelopes != 0 {
            self.mod_envelopes
                .advance_voice(v, &block.envelope_coefs, usage.envelopes)
        } else {
            ([0.0; MAX_ENVELOPES], [0.0; MAX_ENVELOPES])
        };
        let (base0, base1) = (
            SourceValues {
                envelope: env0,
                ..base0
            },
            SourceValues {
                envelope: env1,
                ..base1
            },
        );
        let (lfo0, lfo1) = if usage.lfos != 0 {
            // Before the LFOs run, their own routes see everything but LFOs.
            let lfos = self.patch.lfos.as_slice();
            let early = (
                routes.lfo_offsets(&block.slots, &base0),
                routes.lfo_offsets(&block.slots, &base1),
            );
            let modulated = self.modulated.lfos();
            self.lfos.advance_voice(
                v,
                lfos,
                usage.lfos,
                modulated,
                (&early.0, &early.1),
                block.block_seconds,
            )
        } else {
            ([0.0; MAX_LFOS], [0.0; MAX_LFOS])
        };
        (
            routes.offsets(&block.slots, &SourceValues { lfo: lfo0, ..base0 }),
            routes.offsets(&block.slots, &SourceValues { lfo: lfo1, ..base1 }),
        )
    }

    /// The voice's master volume (with its modulation), and in stereo its
    /// balance and unison spread, as gains ramped across the block.
    fn voice_amp<const STEREO: bool>(
        &self,
        v: usize,
        block: &Block,
        (o0, o1): (&Offsets, &Offsets),
    ) -> Ramp {
        let amp = (block.volume0 + o0.get(ModTarget::Volume)).clamp(0.0, 1.0);
        let amp_end = (self.volume + o1.get(ModTarget::Volume)).clamp(0.0, 1.0);
        let step = (amp_end - amp) * block.inv_ns;
        if !STEREO {
            return Ramp {
                gain: (amp, amp),
                step: (step, step),
            };
        }
        let layer = self.layer[v].position();
        let (l0, r0) =
            balance_gains(block.balance0 + o0.get(ModTarget::Balance) + layer * block.spread0);
        let (l1, r1) =
            balance_gains(self.balance + o1.get(ModTarget::Balance) + layer * self.spread);
        let (amp_l, amp_r) = (amp * l0, amp * r0);
        Ramp {
            gain: (amp_l, amp_r),
            step: (
                (amp_end * l1 - amp_l) * block.inv_ns,
                (amp_end * r1 - amp_r) * block.inv_ns,
            ),
        }
    }

    /// Per operator for this voice and block: advances the envelopes and
    /// sets up the phase increments, ramped gains, feedback and pans.
    fn load_operators<const STEREO: bool>(
        &mut self,
        v: usize,
        note: f32,
        block: &Block,
        depths: &[[f32; OPERATORS]; OPERATORS],
        (o0, o1): (&Offsets, &Offsets),
    ) -> OpLanes {
        let routing = &block.routing;
        let freq = pitch::note_to_freq(note + self.layer[v].position() * self.patch.detune * CENTS);
        // Semitones for the operators that follow the note.
        let tune = block.tune0 + o0.get(ModTarget::Tune);
        let mut lanes = OpLanes::default();
        for (i, op) in self.operators.iter_mut().enumerate() {
            let params = &self.patch.operators[i];
            let (level0, level1) = block.levels[i];
            let env0 = op.envelope[v].level();
            let scaling = &params.key_scaling;
            let env1 = if block.op_envelopes & (1 << i) != 0 {
                let coefs = block.coefs[i]
                    .modulated(&params.envelope, |s| o0.get(ModTarget::OpEnvelope(i, s)));
                op.envelope[v].advance_scaled(&coefs, scaling.speed(note))
            } else {
                op.envelope[v].advance_scaled(&block.coefs[i], scaling.speed(note))
            };
            let key_gain = scaling.gain(note);
            lanes.phases[i] = op.phase[v];
            let (base, semitones) = match params.pitch_mode {
                PitchMode::Ratio => (freq * op.ratio, tune),
                PitchMode::Semitone => (freq, tune + op.semitones),
                PitchMode::Frequency => (op.frequency, 0.0),
            };
            let semitones = semitones + o0.get(ModTarget::OpPitch(i));
            let bend = if semitones != 0.0 {
                klang_dsp::units::semitones_to_ratio(semitones)
            } else {
                1.0
            };
            let bend = if block.ratios & (1 << i) != 0 {
                bend * (1.0 + o0.get(ModTarget::OpRatio(i))).max(0.0)
            } else {
                bend
            };
            lanes.incs[i] = phase::increment(base * bend, block.rate);
            lanes.dts[i] = phase::to_unit(lanes.incs[i]);
            lanes.gains[i] =
                env0 * ((level0 + o0.get(ModTarget::OpLevel(i))) * key_gain).clamp(0.0, 1.0);
            lanes.gain_steps[i] = (env1
                * ((level1 + o1.get(ModTarget::OpLevel(i))) * key_gain).clamp(0.0, 1.0)
                - lanes.gains[i])
                * block.inv_ns;
            lanes.feedback[i] =
                (op.feedback + o0.get(ModTarget::OpFeedback(i))).clamp(0.0, 1.0) * MAX_FEEDBACK;
            if block.folding & (1 << i) != 0 {
                let (fold0, fold1) = block.folds[i];
                lanes.folds[i] = (fold0 + o0.get(ModTarget::OpFold(i))).clamp(0.0, 1.0) * MAX_FOLD;
                let end = (fold1 + o1.get(ModTarget::OpFold(i))).clamp(0.0, 1.0) * MAX_FOLD;
                lanes.fold_steps[i] = (end - lanes.folds[i]) * block.inv_ns;
            }
            lanes.out1[i] = op.out1[v];
            lanes.noise[i] = op.noise[v];
            lanes.out2[i] = op.out2[v];
            if STEREO && routing.outputs & (1 << i) != 0 {
                let (pan0, pan1) = block.pans[i];
                let (l0, r0) = balance_gains(pan0 + o0.get(ModTarget::OpPan(i)));
                let (l1, r1) = balance_gains(pan1 + o1.get(ModTarget::OpPan(i)));
                lanes.pan.gain[i] = (l0, r0);
                lanes.pan.step[i] = ((l1 - l0) * block.inv_ns, (r1 - r0) * block.inv_ns);
            }
        }
        // Sync speeds the carrier up by its sources' levels (block rate).
        for (i, depths) in depths.iter().enumerate() {
            if routing.sync[i] != 0 {
                let octaves: f32 = bits(routing.sync[i])
                    .map(|j| lanes.gains[j] * depths[j] * MAX_SYNC_OCTAVES)
                    .sum();
                // Wraps like `phase::increment` for very high pitches.
                lanes.incs[i] = (lanes.incs[i] as f32 * libm::exp2f(octaves)) as u64 as u32;
                lanes.dts[i] = phase::to_unit(lanes.incs[i]);
            }
        }
        lanes.inv_incs = core::array::from_fn(|i| 1.0 / lanes.incs[i].max(1) as f32);
        // Filter operators: the pitch is the cutoff (block rate, like pitch).
        for i in bits(block.filter_ops) {
            let [ic1, ic2] = self.operators[i].filter[v];
            let c = Coefs::new(
                warp(lanes.dts[i] * block.rate, block.rate),
                block.dampings[i],
            );
            (lanes.svf_ic1[i], lanes.svf_ic2[i]) = (ic1, ic2);
            (lanes.svf_a1[i], lanes.svf_a2[i], lanes.svf_a3[i]) = (c.a1, c.a2, c.a3);
        }
        lanes
    }

    /// Runs the voice's operator output through its filters.
    fn filter_voice<const STEREO: bool>(
        &mut self,
        v: usize,
        filters: &mut [FilterRun; FILTERS],
        len: usize,
    ) {
        self.ringing &= !(1 << v);
        if filters.iter().all(|f| f.kind == FilterKind::Off) {
            return;
        }
        let (voice, voice_r) = (&mut self.voice[..len], &mut self.voice_r[..len]);
        for (f, run) in filters.iter_mut().enumerate() {
            let state = &mut self.filter_state[f][v];
            if STEREO {
                // Both channels with the same coefficient ramp.
                let state_r = &mut self.filter_state_r[f][v];
                for (x, x_r) in voice.iter_mut().zip(voice_r.iter_mut()) {
                    *x = run.filter(state, *x);
                    *x_r = run.filter(state_r, *x_r);
                    run.advance();
                }
            } else {
                for x in voice.iter_mut() {
                    *x = run.filter(state, *x);
                    run.advance();
                }
            }
        }
        let ringing = (0..FILTERS).any(|f| {
            self.filter_state[f][v].ringing() || STEREO && self.filter_state_r[f][v].ringing()
        });
        if ringing {
            self.ringing |= 1 << v;
        }
    }

    /// Adds the filtered voice to the mix with its volume ramp.
    fn mix_voice<const STEREO: bool>(&mut self, amp: Ramp, len: usize) {
        let Ramp {
            gain: (mut amp_l, mut amp_r),
            step: (step_l, step_r),
        } = amp;
        let (mix, mix_r) = if self.oversampling == 2 {
            (
                &mut self.mix_oversampled[..len],
                &mut self.mix_oversampled_r[..len],
            )
        } else {
            (&mut self.mix[..len], &mut self.mix_r[..len])
        };
        let (voice, voice_r) = (&self.voice[..len], &self.voice_r[..len]);
        if STEREO {
            for ((out, out_r), (&x, &x_r)) in mix
                .iter_mut()
                .zip(mix_r.iter_mut())
                .zip(voice.iter().zip(voice_r))
            {
                *out += x * amp_l;
                *out_r += x_r * amp_r;
                amp_l += step_l;
                amp_r += step_r;
            }
        } else {
            for (out, &x) in mix.iter_mut().zip(voice) {
                *out += x * amp_l;
                amp_l += step_l;
            }
        }
    }
}

/// Cents per semitone.
const CENTS: f32 = 0.01;

/// A gain pair (left, right) ramped linearly across a block.
#[derive(Clone, Copy, Default)]
struct Ramp {
    gain: (f32, f32),
    step: (f32, f32),
}

/// One voice's operators while it renders a block: one lane per operator,
/// loaded from and stored back to [`crate::operator::Operator`].
#[derive(Default)]
struct OpLanes {
    phases: [u32; OPERATORS],
    /// Phase increment, as a fraction of a period (for the anti-aliasing),
    /// and its reciprocal.
    incs: [u32; OPERATORS],
    dts: [f32; OPERATORS],
    inv_incs: [f32; OPERATORS],
    /// Level × envelope, ramped linearly across the block.
    gains: [f32; OPERATORS],
    gain_steps: [f32; OPERATORS],
    feedback: [f32; OPERATORS],
    /// Fold amount, 0..=[`MAX_FOLD`], ramped linearly across the block.
    folds: [f32; OPERATORS],
    fold_steps: [f32; OPERATORS],
    /// The last two output samples.
    out1: [f32; OPERATORS],
    out2: [f32; OPERATORS],
    noise: [u32; OPERATORS],
    /// Filter operators: the SVF's integrators and coefficients.
    svf_ic1: [f32; OPERATORS],
    svf_ic2: [f32; OPERATORS],
    svf_a1: [f32; OPERATORS],
    svf_a2: [f32; OPERATORS],
    svf_a3: [f32; OPERATORS],
    /// Per heard operator in stereo: left and right gain.
    pan: Pans,
}

/// Left and right gain per operator, ramped; unity in mono.
struct Pans {
    gain: [(f32, f32); OPERATORS],
    step: [(f32, f32); OPERATORS],
}

impl Default for Pans {
    fn default() -> Self {
        Self {
            gain: [(1.0, 1.0); OPERATORS],
            step: [(0.0, 0.0); OPERATORS],
        }
    }
}

impl OpLanes {
    /// Writes the state that carries over to the next block back.
    fn store(&self, operators: &mut [crate::operator::Operator; OPERATORS], v: usize) {
        for (i, op) in operators.iter_mut().enumerate() {
            op.phase[v] = self.phases[i];
            op.out1[v] = self.out1[i];
            op.out2[v] = self.out2[i];
            op.noise[v] = self.noise[i];
            op.filter[v] = [self.svf_ic1[i], self.svf_ic2[i]];
        }
    }
}

/// The operators of [`LANES`] voices while they render a block, one
/// voice per lane: [`OpLanes`] transposed.
struct OpVectors {
    phases: [U32x4; OPERATORS],
    incs: [U32x4; OPERATORS],
    dts: [F32x4; OPERATORS],
    inv_incs: [F32x4; OPERATORS],
    gains: [F32x4; OPERATORS],
    gain_steps: [F32x4; OPERATORS],
    feedback: [F32x4; OPERATORS],
    folds: [F32x4; OPERATORS],
    fold_steps: [F32x4; OPERATORS],
    out1: [F32x4; OPERATORS],
    out2: [F32x4; OPERATORS],
    noise: [U32x4; OPERATORS],
    svf_ic1: [F32x4; OPERATORS],
    svf_ic2: [F32x4; OPERATORS],
    svf_a1: [F32x4; OPERATORS],
    svf_a2: [F32x4; OPERATORS],
    svf_a3: [F32x4; OPERATORS],
    /// Left and right gain per operator, and their steps.
    pan_gain: [(F32x4, F32x4); OPERATORS],
    pan_step: [(F32x4, F32x4); OPERATORS],
}

impl OpVectors {
    fn gather(lanes: &[OpLanes; LANES]) -> Self {
        let f = |field: fn(&OpLanes) -> &[f32; OPERATORS]| -> [F32x4; OPERATORS] {
            core::array::from_fn(|i| {
                F32x4::from_array(core::array::from_fn(|k| field(&lanes[k])[i]))
            })
        };
        let u = |field: fn(&OpLanes) -> &[u32; OPERATORS]| -> [U32x4; OPERATORS] {
            core::array::from_fn(|i| {
                U32x4::from_array(core::array::from_fn(|k| field(&lanes[k])[i]))
            })
        };
        let pair =
            |field: fn(&OpLanes) -> &[(f32, f32); OPERATORS]| -> [(F32x4, F32x4); OPERATORS] {
                core::array::from_fn(|i| {
                    let left = core::array::from_fn(|k| field(&lanes[k])[i].0);
                    let right = core::array::from_fn(|k| field(&lanes[k])[i].1);
                    (F32x4::from_array(left), F32x4::from_array(right))
                })
            };
        Self {
            phases: u(|l| &l.phases),
            incs: u(|l| &l.incs),
            dts: f(|l| &l.dts),
            inv_incs: f(|l| &l.inv_incs),
            gains: f(|l| &l.gains),
            gain_steps: f(|l| &l.gain_steps),
            feedback: f(|l| &l.feedback),
            folds: f(|l| &l.folds),
            fold_steps: f(|l| &l.fold_steps),
            out1: f(|l| &l.out1),
            out2: f(|l| &l.out2),
            noise: u(|l| &l.noise),
            svf_ic1: f(|l| &l.svf_ic1),
            svf_ic2: f(|l| &l.svf_ic2),
            svf_a1: f(|l| &l.svf_a1),
            svf_a2: f(|l| &l.svf_a2),
            svf_a3: f(|l| &l.svf_a3),
            pan_gain: pair(|l| &l.pan.gain),
            pan_step: pair(|l| &l.pan.step),
        }
    }

    /// Writes the state that carries over to the next block back into the
    /// lanes (see [`OpLanes::store`]).
    fn scatter(&self, lanes: &mut [OpLanes; LANES]) {
        for i in 0..OPERATORS {
            let (phases, out1) = (self.phases[i].to_array(), self.out1[i].to_array());
            let (out2, noise) = (self.out2[i].to_array(), self.noise[i].to_array());
            let (ic1, ic2) = (self.svf_ic1[i].to_array(), self.svf_ic2[i].to_array());
            for (k, lane) in lanes.iter_mut().enumerate() {
                lane.phases[i] = phases[k];
                lane.out1[i] = out1[k];
                lane.out2[i] = out2[k];
                lane.noise[i] = noise[k];
                (lane.svf_ic1[i], lane.svf_ic2[i]) = (ic1[k], ic2[k]);
            }
        }
    }
}

/// Modulation depths of [`LANES`] voices, one voice per lane: `depths[i][j]`
/// of source `j` on target `i`, and the same times [`MAX_MODULATION`] for
/// phase and frequency modulation (see [`Block::depths`]).
struct Depths {
    depths: [[F32x4; OPERATORS]; OPERATORS],
    pm: [[F32x4; OPERATORS]; OPERATORS],
}

impl Depths {
    fn gather(lanes: &[[[f32; OPERATORS]; OPERATORS]; LANES]) -> Self {
        let lane = |scale: f32| -> [[F32x4; OPERATORS]; OPERATORS] {
            core::array::from_fn(|i| {
                core::array::from_fn(|j| {
                    F32x4::from_array(core::array::from_fn(|k| lanes[k][i][j] * scale))
                })
            })
        };
        Self {
            depths: lane(1.0),
            pm: lane(MAX_MODULATION),
        }
    }

    /// The same depths in every lane.
    fn splat(depths: &[[f32; OPERATORS]; OPERATORS]) -> Self {
        Self {
            depths: depths.map(|row| row.map(F32x4::splat)),
            pm: depths.map(|row| row.map(|d| F32x4::splat(d * MAX_MODULATION))),
        }
    }
}

/// The sample loop of [`LANES`] voices: every active operator per sample,
/// highest first, summed into `voice` (and `voice_r` in stereo), times
/// `pre`. Per lane the same arithmetic in the same order as one voice in
/// scalar code, so the result does not depend on the grouping. Without
/// `SHAPED` (no operator folds or filters in this block) the checks for
/// both are compiled out: on the Pi they cost up to 14 % otherwise.
#[inline(always)]
fn render_operators<const STEREO: bool, const SHAPED: bool>(
    ops: &mut OpVectors,
    block: &Block,
    depths: &Depths,
    pre: F32x4,
    voice: &mut [[f32; LANES]],
    voice_r: &mut [[f32; LANES]],
) {
    let routing = &block.routing;
    let s = F32x4::splat;
    let OpVectors {
        phases,
        incs,
        dts,
        inv_incs,
        gains,
        gain_steps,
        feedback,
        folds,
        fold_steps,
        out1,
        out2,
        noise,
        svf_ic1,
        svf_ic2,
        svf_a1,
        svf_a2,
        svf_a3,
        pan_gain,
        pan_step,
    } = ops;
    let min_cutoff = MIN_CUTOFF_HZ / block.rate;
    // The phase step each operator took last (frequency modulation
    // changes it per sample), for sync's wrap detection.
    let mut steps = *incs;
    // Only the left buffer is iterated; the right one is written by
    // index in the stereo version alone.
    for (k, out) in voice.iter_mut().enumerate() {
        let mut sample = F32x4::ZERO;
        let mut sample_r = F32x4::ZERO;
        // Highest operator first: a source with a higher index has
        // already produced this sample, a lower one (and the operator
        // itself, for feedback) contributes its previous sample.
        for i in (0..OPERATORS).rev() {
            if routing.active & (1 << i) == 0 {
                continue;
            }
            let is_filter = SHAPED && block.filter_ops & (1 << i) != 0;
            let wave = if is_filter {
                // The phase-mode sources are the filter's input.
                let mut x = F32x4::ZERO;
                for j in bits(routing.phase[i]) {
                    x = x + out1[j] * depths.depths[i][j];
                }
                let k = s(block.dampings[i]);
                let (a1, a2, a3) = if routing.frequency[i] != 0 {
                    // Filter FM: the frequency-mode sources move the cutoff,
                    // scaled like linear FM of an oscillator at this pitch.
                    let f = bits(routing.frequency[i])
                        .fold(dts[i], |f, j| f + out1[j] * depths.pm[i][j] * dts[j]);
                    let g = tan_pi(f.clamp(min_cutoff, MAX_CUTOFF));
                    let a1 = s(1.0) / (s(1.0) + g * (g + k));
                    let a2 = g * a1;
                    (a1, a2, g * a2)
                } else {
                    (svf_a1[i], svf_a2[i], svf_a3[i])
                };
                // One TPT state-variable step, as `klang_dsp::svf::Svf::tick`.
                let v3 = x - svf_ic2[i];
                let v1 = a1 * svf_ic1[i] + a2 * v3;
                let v2 = svf_ic2[i] + a2 * svf_ic1[i] + a3 * v3;
                svf_ic1[i] = s(2.0) * v1 - svf_ic1[i];
                svf_ic2[i] = s(2.0) * v2 - svf_ic2[i];
                match block.kinds[i] {
                    OperatorKind::HighPass => x - k * v1 - v2,
                    OperatorKind::BandPass => k * v1,
                    OperatorKind::LowPass | OperatorKind::Oscillator => v2,
                }
            } else {
                // Feedback averages the last two samples to stay stable.
                let mut modulation = feedback[i] * s(0.5) * (out1[i] + out2[i]);
                for j in bits(routing.phase[i]) {
                    modulation = modulation + out1[j] * depths.pm[i][j];
                }
                // `phase::from_radians` per lane.
                let mut p = phases[i]
                    .wrapping_add((modulation * s(phase::PHASE_PER_RADIAN)).to_u32_wrapping());
                for j in bits(routing.pulse_width[i]) {
                    let depth = gains[j] * depths.depths[i][j];
                    p = p.wrapping_add(pulse_width_offset(
                        phases[j],
                        depth,
                        incs[i].to_f32() * inv_incs[j],
                    ));
                }
                match block.waveforms[i] {
                    Waveform::Sine => sine4(p),
                    Waveform::Noise => waveform::noise4(&mut noise[i]),
                    other => waveform::sample4(other, p, dts[i]),
                }
            };
            let wave = if SHAPED && block.folding & (1 << i) != 0 {
                let folded = fold(wave, folds[i]);
                folds[i] = folds[i] + fold_steps[i];
                folded
            } else {
                wave
            };
            let mut y = wave * gains[i];
            for j in bits(routing.ring[i]) {
                let depth = depths.depths[i][j];
                y = y * (s(1.0) - gains[j] * depth + out1[j] * depth);
            }
            out2[i] = out1[i];
            out1[i] = y;
            if routing.outputs & (1 << i) != 0 {
                if STEREO {
                    let (l, r) = pan_gain[i];
                    sample = sample + y * l;
                    sample_r = sample_r + y * r;
                    pan_gain[i] = (l + pan_step[i].0, r + pan_step[i].1);
                } else {
                    sample = sample + y;
                }
            }
            // Frequency modulation and sync act on the phase itself.
            let mut inc = incs[i];
            // (A filter's frequency-mode sources moved its cutoff instead.)
            if routing.frequency[i] != 0 && !is_filter {
                // Starts at -0.0 like `f32`'s `Sum`.
                let deviation = bits(routing.frequency[i]).fold(s(-0.0), |sum, j| {
                    sum + out1[j] * depths.pm[i][j] * incs[j].to_f32()
                });
                inc = inc.wrapping_add(deviation.to_u32_wrapping());
            }
            phases[i] = phases[i].wrapping_add(inc);
            steps[i] = inc;
            for j in bits(routing.sync[i]) {
                // The modulator wrapped forwards in its last step: restart,
                // keeping the fraction of a sample since the wrap. (Running
                // backwards, through-zero FM, it does not.)
                let step = steps[j];
                let wrapped = step.positive_i32() & phases[j].lt(step);
                if wrapped.any() {
                    let at = phases[j].to_f32();
                    let since = step
                        .eq(incs[j])
                        .select(at * inv_incs[j], at / step.to_f32());
                    phases[i] = wrapped.select_u32((since * incs[i].to_f32()).to_u32(), phases[i]);
                }
            }
            gains[i] = gains[i] + gain_steps[i];
        }
        *out = (sample * pre).to_array();
        if STEREO {
            voice_r[k] = (sample_r * pre).to_array();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold1(x: f32, a: f32) -> f32 {
        fold(F32x4::splat(x), F32x4::splat(a)).to_array()[0]
    }

    #[test]
    fn fold_is_exact_at_zero_and_continuous() {
        for i in -100..=100 {
            let x = i as f32 * 0.0123;
            assert_eq!(fold1(x, 0.0).to_bits(), x.to_bits(), "{x}");
            // Gliding through the kink at 1 does not jump.
            assert!(
                (fold1(x, 1.0 - 1e-4) - fold1(x, 1.0 + 1e-4)).abs() < 1e-3,
                "{x}"
            );
        }
    }

    #[test]
    fn fold_follows_the_sine_and_stays_within_one() {
        for i in -100..=100 {
            let x = i as f32 * 0.01;
            for a in [1.0, 2.5, MAX_FOLD] {
                let expected = libm::sinf(core::f32::consts::FRAC_PI_2 * a * x);
                assert!((fold1(x, a) - expected).abs() < 1e-4, "{x} {a}");
            }
            for a in [0.0, 0.3, 1.0, 3.0, MAX_FOLD] {
                assert!(fold1(x, a).abs() <= 1.0 + 1e-5, "{x} {a}");
            }
        }
        // Three quarter periods: a full-scale peak folds over to the far side.
        assert!((fold1(1.0, 3.0) + 1.0).abs() < 1e-4);
    }

    #[test]
    fn tan_pi_is_close_up_to_the_highest_cutoff() {
        for i in 1..=450 {
            let f = i as f32 * 0.001;
            let exact = libm::tanf(core::f32::consts::PI * f);
            let approx = tan_pi(F32x4::splat(f)).to_array()[0];
            let tolerance = if f <= 0.25 { 1e-3 } else { 0.035 };
            assert!(
                (approx / exact - 1.0).abs() < tolerance,
                "{f}: {approx} vs {exact}"
            );
        }
    }
}
