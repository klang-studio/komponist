//! [`HexaProcessor`]: hexa's `Synth<1>` as an in-process engine [`Processor`].

use hexa_core::midi::CC_ALL_NOTES_OFF;
use hexa_core::oversampling::DELAY;
use hexa_core::{MidiMessage, Patch, Synth};
use komponist_device_core::{INITIAL_RATE, SAMPLE_RATES};
use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};

use crate::device::Field;
use crate::state::{self, StateError};

// `activate` overwrites the synth in place without dropping it; that is only sound
// while `Synth` owns nothing that needs dropping (it is plain `no_std` data).
const _: () = assert!(!core::mem::needs_drop::<Synth<1>>());

/// hexa as an instrument in a komponist chain (ARCHITECTURE §8.5).
///
/// Wraps one `hexa_core::Synth<1>` on the heap (about 2 MB, mostly the master effects' delay
/// lines). Create it on the UI thread with [`HexaProcessor::new`] or
/// [`HexaProcessor::from_state`], activate it, send it to the engine.
///
/// Events (all applied at their exact frame by splitting the render at event offsets):
/// - `NoteOn` / `NoteOff`: `Synth::note_on` / `note_off` (omni: channel and note id are not
///   used; hexa pairs notes by key).
/// - `Midi`: `Synth::midi` (sustain pedal, mod wheel, pitch bend, aftertouch, CC 120/123).
/// - `Param`: a [`Field`] by wire id: one of the 8 macros (0–7) or a parameter of the device card
///   ([`crate::device`]); unknown ids are ignored. Parameter changes at one offset are
///   collected on a copy of the patch and applied with one `Synth::set_patch` before the next
///   rendered frame or note (hexa glides ratios, levels and volume over 10 ms).
/// - `AllNotesOff`: MIDI All Notes Off: every key and the sustain pedal are released, the
///   voices go through their release (the engine contract: "release all sounding notes").
///
/// [`Processor::reset`] silences at once: voices, keys, pedal, effect tails and the
/// oversampling filter. It keeps the patch and invalidates the effect delay lines without
/// writing their sample buffers.
///
/// [`Processor::latency`] is the delay of hexa's 2× oversampling filter,
/// [`hexa_core::oversampling::DELAY`] = 17 frames at any sample rate: a note-on at offset `n`
/// is heard from frame `n + 17` on (the filter's pre-ringing, below −100 dBFS, starts at `n`).
///
/// Output is stereo and clamped to −1 ..= 1 by hexa; `ctx.offline` changes nothing (hexa
/// always renders at its full quality).
pub struct HexaProcessor {
    synth: Box<Synth<1>>,
    sample_rate: f32,
    /// The patch with the parameter changes of the current offset, while `dirty`.
    edit: Patch,
    dirty: bool,
}

impl HexaProcessor {
    /// A processor playing `patch` (sanitized). Allocates the synth; not RT-safe.
    pub fn new(patch: Patch) -> HexaProcessor {
        let patch = patch.sanitized();
        HexaProcessor {
            synth: Synth::boxed(INITIAL_RATE, patch),
            sample_rate: INITIAL_RATE,
            edit: patch,
            dirty: false,
        }
    }

    /// A processor playing the patch in device state `state` (`None`: "Init"); see
    /// [`crate::decode_state`].
    pub fn from_state(state: Option<&[u8]>) -> Result<HexaProcessor, StateError> {
        state::decode_state(state).map(HexaProcessor::new)
    }

    /// The patch playing now, with all parameter changes received so far.
    pub fn patch(&self) -> &Patch {
        self.synth.patch()
    }

    /// The sample rate the processor was last activated at (48 kHz before that).
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Voices sounding now (attack to the end of the release).
    pub fn active_voices(&self) -> usize {
        self.synth.active_voices()
    }

    /// Initializes all storage at the new sample rate during activation.
    fn rebuild(&mut self) {
        let patch = *self.synth.patch();
        let place: *mut Synth<1> = &mut *self.synth;
        // SAFETY: `place` comes from a live `&mut`, so it is valid for writes and aligned, and
        // nothing else uses the synth meanwhile. The old value is overwritten without being
        // dropped, which leaks nothing: `Synth<1>` has no drop glue (asserted above).
        unsafe { Synth::write_new(place, self.sample_rate, patch) };
    }

    /// Replace the complete patch, retaining values outside the device card's controls.
    /// Uses the existing synth's patch-update semantics and is allocation-free.
    pub fn replace_patch(&mut self, patch: Patch) {
        self.dirty = false;
        self.synth.set_patch(patch);
    }

    /// Render one ordered span for either the built-in engine or the CLAP adapter.
    /// An empty span applies its frame-zero events without advancing the synth.
    pub fn render_events(&mut self, events: &[Event], [left, right]: [&mut [f32]; 2]) {
        let frames = left.len();
        let mut done = 0;
        for event in events {
            let at = (event.offset as usize).clamp(done, frames);
            if at > done {
                self.flush();
                self.synth.render(&mut left[done..at], &mut right[done..at]);
                done = at;
            }
            self.apply(event.kind);
        }
        self.flush();
        if done < frames {
            self.synth.render(&mut left[done..], &mut right[done..]);
        }
    }

    /// Applies collected parameter changes.
    fn flush(&mut self) {
        if self.dirty {
            self.dirty = false;
            self.synth.set_patch(self.edit);
        }
    }

    /// Applies one event at the current frame.
    fn apply(&mut self, kind: EventKind) {
        if let EventKind::Param { id, value } = kind {
            if let Some(field) = Field::from_wire(id) {
                if !self.dirty {
                    self.edit = *self.synth.patch();
                    self.dirty = true;
                }
                field.set(&mut self.edit, value);
            }
            return;
        }
        // Notes start with the patch as changed by the parameters before them.
        self.flush();
        match kind {
            EventKind::NoteOn { key, velocity, .. } => self.synth.note_on(key, velocity),
            EventKind::NoteOff { key, .. } => self.synth.note_off(key),
            EventKind::Midi(bytes) => self.synth.midi(&bytes),
            EventKind::AllNotesOff => self.synth.handle_midi(MidiMessage::ControlChange {
                channel: 0,
                controller: CC_ALL_NOTES_OFF,
                value: 0,
            }),
            EventKind::Param { .. } => {}
        }
    }
}

impl Default for HexaProcessor {
    /// A processor playing "Init".
    fn default() -> HexaProcessor {
        HexaProcessor::new(state::default_patch())
    }
}

impl Processor for HexaProcessor {
    fn activate(&mut self, sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        if !SAMPLE_RATES.contains(&sample_rate) {
            return Err(ProcessorError::UnsupportedSampleRate(sample_rate));
        }
        self.flush();
        self.sample_rate = sample_rate;
        self.rebuild();
        Ok(())
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let [left, right] = &mut io.output;
        self.render_events(events, [left, right]);
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        if self.synth.oversampling() > 1 {
            DELAY as u32
        } else {
            0
        }
    }

    fn reset(&mut self) {
        self.flush();
        self.synth.reset();
    }
}
