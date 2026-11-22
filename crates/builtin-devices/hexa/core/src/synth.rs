//! The complete synthesizer and the only way to play the engine: keys,
//! pedal and voice allocation ([`Voicing`]), the voices split over several
//! engines, so each can render on its own CPU core, and the master effects
//! on their sum.
//!
//! One allocator decides for all [`MAX_VOICES`] voices; voice `v` then
//! sounds on engine `v % N`. Since the voice
//! allocator takes the first idle voice, chords spread evenly. Every engine
//! gets the same patch, settings and blocks, so the per-block state outside
//! the voices (smoothing, free-running LFOs) stays identical, and the sum of
//! the engines' unclamped outputs equals that of `Synth<1>` with all voices
//! on one engine (up to the order of the float additions and
//! sample-and-hold random values). The
//! effects ([`Effects`]) run on that sum.

use core::ptr::addr_of_mut;

use crate::allocator::Allocation;
use crate::effects::Effects;
use crate::engine::{EnvelopePositions, clamp};
use crate::lfo::LfoPositions;
use crate::midi::MidiMessage;
use crate::modulation::ModulationValues;
use crate::voicing::{Layer, Voices, Voicing};
use crate::{BLOCK_SIZE, Engine, MAX_VOICES, Patch};

/// `N` engines sharing the voices plus the master effects; see the module
/// documentation. About 2 MB (the effects' delay lines), so it is only built
/// in place: [`Synth::write_new`] into static memory or a heap allocation
/// ([`Synth::boxed`] with the `alloc` feature).
pub struct Synth<const N: usize> {
    engines: [Engine; N],
    /// Keys, pedal and voice allocation for all engines.
    voicing: Voicing,
    effects: Effects,
}

impl<const N: usize> Synth<N> {
    const VALID: () = assert!(N >= 1 && N <= MAX_VOICES, "1..=MAX_VOICES engines");

    /// Writes a new synth with `patch` to `this` without building it on the
    /// stack (only one engine at a time is).
    ///
    /// # Safety
    /// `this` must be valid for writes and suitably aligned.
    pub unsafe fn write_new(this: *mut Self, sample_rate: f32, patch: Patch) {
        let () = Self::VALID;
        // SAFETY: guaranteed by the caller; every field is written.
        unsafe {
            for k in 0..N {
                addr_of_mut!((*this).engines[k]).write(Self::new_engine(k, sample_rate, patch));
            }
            addr_of_mut!((*this).voicing).write(Voicing::new());
            Effects::write_new(addr_of_mut!((*this).effects), sample_rate);
        }
    }

    fn new_engine(index: usize, sample_rate: f32, patch: Patch) -> Engine {
        let mut engine = Engine::with_patch(sample_rate, patch);
        if index > 0 {
            engine.seed(0x9e37_79b9u32.wrapping_mul(index as u32 + 1));
        }
        engine
    }

    /// Silences the voices and effects while keeping the current patch and configuration.
    /// Effect delay lines are cleared logically, without rewriting their sample storage.
    pub fn reset(&mut self) {
        let patch = *self.patch();
        let sample_rate = self.engines[0].sample_rate();
        let oversampling = self.oversampling();
        for (index, engine) in self.engines.iter_mut().enumerate() {
            *engine = Self::new_engine(index, sample_rate, patch);
            engine.set_oversampling(oversampling);
        }
        self.voicing = Voicing::new();
        self.effects.reset();
    }

    /// A new synth with `patch` on the heap, built in place.
    #[cfg(any(test, feature = "alloc"))]
    pub fn boxed(sample_rate: f32, patch: Patch) -> alloc::boxed::Box<Self> {
        let mut synth = alloc::boxed::Box::<Self>::new_uninit();
        // SAFETY: a valid place, fully initialized by `write_new`.
        unsafe {
            Self::write_new(synth.as_mut_ptr(), sample_rate, patch);
            synth.assume_init()
        }
    }

    /// Engine `k`, for tests of its voices.
    #[cfg(test)]
    pub(crate) fn engine(&self, k: usize) -> &Engine {
        &self.engines[k]
    }

    /// The engine that renders voice `v`.
    fn owner(v: usize) -> usize {
        v % N
    }

    /// Bit mask of the voices engine `k` renders.
    fn owned(k: usize) -> u32 {
        (0..MAX_VOICES)
            .filter(|&v| Self::owner(v) == k)
            .fold(0, |mask, v| mask | 1 << v)
    }

    /// Renders the operators at `factor` × the sample rate: 2 (the default)
    /// keeps the harmonics of strong FM and saw/square waves from aliasing
    /// up to twice the output's Nyquist frequency, at about twice the CPU
    /// cost; 1 renders directly. Other values are clamped to that range.
    /// At 2 the output is delayed by [`crate::oversampling::DELAY`] samples.
    pub fn set_oversampling(&mut self, factor: usize) {
        self.engines
            .iter_mut()
            .for_each(|e| e.set_oversampling(factor));
    }

    pub fn oversampling(&self) -> usize {
        self.engines[0].oversampling()
    }

    pub fn patch(&self) -> &Patch {
        self.engines[0].patch()
    }

    /// Changes the patch. Ratios, levels and volume glide to the new values.
    /// Out-of-range values are clamped, see [`Patch::sanitized`].
    pub fn set_patch(&mut self, patch: Patch) {
        let patch = patch.sanitized();
        self.engines.iter_mut().for_each(|e| e.set_patch(patch));
    }

    /// Number of voices currently producing sound, over all engines.
    pub fn active_voices(&self) -> usize {
        self.engines.iter().map(Engine::active_voices).sum()
    }

    /// Envelope positions of the newest sounding voice (by note-on order),
    /// `None` while silent; see [`EnvelopePositions`].
    pub fn envelope_positions(&self) -> Option<EnvelopePositions> {
        let v = self
            .voicing
            .newest(|v| self.engines[Self::owner(v)].is_sounding(v))?;
        Some(self.engines[Self::owner(v)].envelope_positions_of(v))
    }

    /// LFO positions of the newest sounding voice (by note-on order), `None`
    /// while silent; see [`LfoPositions`].
    pub fn lfo_positions(&self) -> Option<LfoPositions> {
        let v = self
            .voicing
            .newest(|v| self.engines[Self::owner(v)].is_sounding(v))?;
        Some(self.engines[Self::owner(v)].lfo_positions_of(v))
    }

    /// Modulation of the newest sounding voice (by note-on order) at the end
    /// of the last block, `None` while silent; for displays that show where
    /// modulated parameters are.
    pub fn modulation(&self) -> Option<ModulationValues> {
        let v = self
            .voicing
            .newest(|v| self.engines[Self::owner(v)].is_sounding(v))?;
        Some(self.engines[Self::owner(v)].modulation_of(v))
    }

    /// Cuts off the oldest sounding voice, when rendering does not keep up
    /// (e.g. at a throttled CPU clock); `false` if none sounds.
    pub fn shed_voice(&mut self) -> bool {
        self.voicing.shed_oldest(&mut Engines(&mut self.engines))
    }

    /// Starts `note` (0..=127, 60 = middle C) with `velocity` (1..=127),
    /// in the patch's voice mode. Larger values are clamped to 127.
    pub fn note_on(&mut self, note: u8, velocity: u8) {
        let params = self.engines[0].voicing_params();
        self.voicing
            .note_on(params, note, velocity, &mut Engines(&mut self.engines));
    }

    /// Releases `note`, or keeps it sounding while the sustain pedal is down.
    pub fn note_off(&mut self, note: u8) {
        let params = self.engines[0].voicing_params();
        self.voicing
            .note_off(params, note, &mut Engines(&mut self.engines));
    }

    /// Sustain pedal: while down, released keys keep sounding until it is
    /// lifted.
    pub fn set_sustain(&mut self, down: bool) {
        self.voicing
            .set_sustain(down, &mut Engines(&mut self.engines));
    }

    /// Handles a MIDI message: notes, the sustain pedal, All Sound Off and
    /// All Notes Off on any channel, and the controllers that modulate (mod
    /// wheel, pitch wheel, aftertouch, see [`crate::controls`]); everything
    /// else is ignored.
    pub fn handle_midi(&mut self, msg: MidiMessage) {
        let params = self.engines[0].voicing_params();
        self.voicing
            .handle_midi(params, msg, &mut Engines(&mut self.engines));
        self.engines.iter_mut().for_each(|e| e.control(&msg));
    }

    /// Decodes and handles one complete raw MIDI message, see
    /// [`MidiMessage::parse`]. Invalid input is ignored.
    pub fn midi(&mut self, bytes: &[u8]) {
        if let Some(msg) = MidiMessage::parse(bytes) {
            self.handle_midi(msg);
        }
    }

    /// Engine `k`, for rendering it with [`Engine::render_unclamped`] on
    /// another core. Every engine must render the same number of frames per
    /// call; the caller sums the outputs and passes them to
    /// [`Self::finish`].
    ///
    /// # Safety
    /// `this` must point to a valid `Synth`, `k < N`, and while the result is
    /// in use nothing else may access that engine (other engines may be
    /// used through this function concurrently).
    pub unsafe fn engine_ptr(this: *mut Self, k: usize) -> *mut Engine {
        assert!(k < N);
        // SAFETY: projects to the field without creating a reference to `*this`.
        unsafe { addr_of_mut!((*this).engines[k]) }
    }

    /// Applies the master effects to the summed engine outputs and clamps
    /// them to -1..1.
    pub fn finish(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let params = self.engines[0].patch().effects;
        self.effects.process(&params, out_l, out_r);
        clamp(out_l);
        clamp(out_r);
    }

    /// Renders all engines one after the other on this core, sums them and
    /// applies [`Self::finish`]. Events take effect at the start of the
    /// call. Only the common length of the two slices is written.
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = out_l.len().min(out_r.len());
        let (out_l, out_r) = (&mut out_l[..frames], &mut out_r[..frames]);
        let (first, rest) = self.engines.split_first_mut().expect("N >= 1");
        first.render_unclamped(out_l, out_r);
        for engine in rest {
            for (l, r) in out_l
                .chunks_mut(BLOCK_SIZE)
                .zip(out_r.chunks_mut(BLOCK_SIZE))
            {
                let mut part_l = [0.0; BLOCK_SIZE];
                let mut part_r = [0.0; BLOCK_SIZE];
                let n = l.len();
                engine.render_unclamped(&mut part_l[..n], &mut part_r[..n]);
                add(l, &part_l[..n]);
                add(r, &part_r[..n]);
            }
        }
        self.finish(out_l, out_r);
    }
}

/// The voices of all engines for [`Voicing`]: voice `v` is on engine `v % N`.
struct Engines<'a, const N: usize>(&'a mut [Engine; N]);

impl<const N: usize> Voices for Engines<'_, N> {
    fn is_sounding(&self, voice: usize) -> bool {
        self.0[voice % N].is_sounding(voice)
    }
    fn start(
        &mut self,
        allocation: Allocation,
        note: u8,
        velocity: u8,
        glide_from: Option<f32>,
        layer: Layer,
    ) {
        let v = allocation.voice();
        self.0[v % N].start(allocation, note, velocity, glide_from, layer);
    }
    fn legato(&mut self, voice: usize, note: u8, glide: bool) {
        self.0[voice % N].legato(voice, note, glide);
    }
    fn release(&mut self, voices: u32) {
        for (k, engine) in self.0.iter_mut().enumerate() {
            engine.release(voices & Synth::<N>::owned(k));
        }
    }
    fn silence(&mut self, voices: u32) {
        for (k, engine) in self.0.iter_mut().enumerate() {
            engine.silence(voices & Synth::<N>::owned(k));
        }
    }
    fn pitch(&self, voice: usize) -> f32 {
        self.0[voice % N].pitch(voice)
    }
}

/// Adds `part` to `sum` sample by sample, see [`klang_dsp::buffer::add`].
pub use klang_dsp::buffer::add;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PRESET_COUNT, preset};

    type Stereo = (Vec<f32>, Vec<f32>);

    fn boxed<const N: usize>(patch: Patch) -> Box<Synth<N>> {
        Synth::boxed(48_000.0, patch)
    }

    /// Plays the same chord with sustain pedal, stealing and releases into
    /// a synth with one engine and into a split; returns both outputs (left,
    /// right).
    fn play<const N: usize>(patch: Patch) -> (Stereo, Stereo) {
        let mut single = boxed::<1>(patch);
        let mut split = boxed::<N>(patch);
        // Values chosen to exercise retrigger, sustain and stealing (> 16 notes).
        let events: &[(usize, &[u8])] = &[
            (0, &[0x90, 48, 100]),
            (0, &[0x90, 55, 80]),
            (1, &[0x90, 60, 127]),
            (2, &[0xb0, 64, 127]),
            (3, &[0x80, 48, 0]),
            (4, &[0x90, 48, 60]),
            (5, &[0x80, 55, 0]),
            (6, &[0xb0, 64, 0]),
            (8, &[0x90, 30, 90]),
        ];
        let (mut a, mut b) = ((Vec::new(), Vec::new()), (Vec::new(), Vec::new()));
        for chunk in 0..120 {
            for &(at, bytes) in events {
                if at == chunk {
                    single.midi(bytes);
                    split.midi(bytes);
                }
            }
            if chunk == 10 {
                for n in 0..20 {
                    single.note_on(70 + n, 100);
                    split.note_on(70 + n, 100);
                }
            }
            if chunk == 40 {
                for n in 0..20 {
                    single.note_off(70 + n);
                    split.note_off(70 + n);
                }
            }
            // An odd length that is not a multiple of the block size.
            let (mut l, mut r) = ([0.0; 100], [0.0; 100]);
            single.render(&mut l, &mut r);
            a.0.extend_from_slice(&l);
            a.1.extend_from_slice(&r);
            split.render(&mut l, &mut r);
            b.0.extend_from_slice(&l);
            b.1.extend_from_slice(&r);
            assert_eq!(
                single.active_voices(),
                split.active_voices(),
                "chunk {chunk}"
            );
        }
        (a, b)
    }

    fn max_difference(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn split_sounds_like_one_engine() {
        // Every preset without sample-and-hold LFOs (their random values
        // differ between the engines by design), without the master effects
        // (they only see the sum).
        let mut compared = 0;
        for i in 0..PRESET_COUNT {
            let patch = Patch {
                effects: Default::default(),
                ..preset(i).unwrap().patch
            };
            if patch
                .lfos
                .as_slice()
                .iter()
                .any(|l| l.waveform == crate::LfoWaveform::SampleHold)
            {
                continue;
            }
            let ((al, ar), (bl, br)) = play::<2>(patch);
            let peak = al.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!(peak > 0.01, "preset {i} silent");
            let diff = max_difference(&al, &bl).max(max_difference(&ar, &br));
            assert!(diff < 1e-5, "preset {i}: difference {diff}");
            compared += 1;
        }
        assert!(compared > 80, "only {compared} presets compared");
    }

    #[test]
    fn three_engines_sound_like_one() {
        let ((al, ar), (bl, br)) = play::<3>(preset(1).unwrap().patch);
        assert!(max_difference(&al, &bl).max(max_difference(&ar, &br)) < 1e-5);
    }

    #[test]
    fn envelope_positions_match_one_engine() {
        let patch = preset(1).unwrap().patch;
        let mut single = boxed::<1>(patch);
        let mut split = boxed::<2>(patch);
        let (mut l, mut r) = ([0.0; 480], [0.0; 480]);
        for note in [60, 64, 67, 71] {
            single.note_on(note, 100);
            split.note_on(note, 100);
            single.render(&mut l, &mut r);
            split.render(&mut l, &mut r);
            // The newest voice (on alternating engines) is the one shown.
            assert_eq!(
                single.envelope_positions(),
                split.envelope_positions(),
                "note {note}"
            );
        }
    }

    #[test]
    fn shedding_cuts_the_oldest_voice() {
        let mut synth = boxed::<2>(Patch::default());
        for note in [48, 52, 55] {
            synth.note_on(note, 100);
        }
        let mut l = [0.0; 256];
        let mut r = [0.0; 256];
        synth.render(&mut l, &mut r);
        assert!(synth.shed_voice());
        assert_eq!(synth.active_voices(), 2);
        // Voice 0 (note 48) went; striking 48 again needs a fresh voice.
        assert!(!synth.engines[0].is_sounding(0));
        assert!(synth.shed_voice() && synth.shed_voice());
        assert!(!synth.shed_voice());
    }

    #[test]
    fn chords_spread_over_the_engines() {
        let mut split = boxed::<2>(Patch::default());
        for n in 0..6 {
            split.note_on(60 + n, 100);
        }
        let (mut l, mut r) = ([0.0; 64], [0.0; 64]);
        split.render(&mut l, &mut r);
        assert_eq!(split.engines[0].active_voices(), 3);
        assert_eq!(split.engines[1].active_voices(), 3);
    }
}
