//! Which voice plays which key: polyphonic allocation, mono and legato
//! with a key stack, the sustain pedal and portamento.
//!
//! [`Voicing`] decides, once for all voices in a [`crate::Synth`]; it acts
//! on them through [`Voices`], which an [`crate::Engine`] implements and
//! the synth spreads over its engines. Mono and legato play voice 0.

use crate::MAX_VOICES;
use crate::allocator::{Allocation, Allocator};
use crate::midi::{
    CC_ALL_NOTES_OFF, CC_ALL_SOUND_OFF, CC_SUSTAIN, DATA_MAX, MidiMessage, SWITCH_ON,
};

/// Longest portamento in seconds.
pub const MAX_PORTAMENTO: f32 = 2.0;

/// Most voices per key (unison).
pub const MAX_UNISON: usize = 8;

/// A voice's place in its key's unison group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layer {
    pub(crate) index: u8,
    pub(crate) count: u8,
}

impl Layer {
    pub(crate) const SINGLE: Self = Self { index: 0, count: 1 };

    /// -1..=1 evenly across the group (0 for a single voice): scales the
    /// detune and the stereo spread.
    pub(crate) fn position(self) -> f32 {
        if self.count <= 1 {
            0.0
        } else {
            2.0 * self.index as f32 / (self.count - 1) as f32 - 1.0
        }
    }
}

/// How keys map to voices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum VoiceMode {
    /// A voice per key; portamento glides from the last played note.
    #[default]
    Poly,
    /// One voice, the envelopes restart with every key (multi-trigger);
    /// portamento always glides.
    Mono,
    /// One voice, the envelopes restart only when no other key was held;
    /// portamento glides only between overlapping keys.
    Legato,
}

/// Patch settings of the voicing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VoicingParams {
    pub(crate) mode: VoiceMode,
    /// Glide time in seconds, 0 = none.
    pub(crate) portamento: f32,
    /// Voices per key, 1..=[`MAX_UNISON`].
    pub(crate) unison: usize,
}

/// What [`Voicing`] needs from the voices.
pub(crate) trait Voices {
    fn is_sounding(&self, voice: usize) -> bool;
    /// Starts `note` on the allocated voice, gliding from `glide_from`
    /// (semitones) if given; `layer` is its place in the key's unison group.
    fn start(
        &mut self,
        allocation: Allocation,
        note: u8,
        velocity: u8,
        glide_from: Option<f32>,
        layer: Layer,
    );
    /// Moves a sounding voice to `note` without restarting its envelopes,
    /// gliding if `glide`.
    fn legato(&mut self, voice: usize, note: u8, glide: bool);
    /// Starts the release of the voices in the bit mask.
    fn release(&mut self, voices: u32);
    /// Cuts the voices in the bit mask off at once.
    fn silence(&mut self, voices: u32);
    /// The voice's current (gliding) pitch in semitones.
    fn pitch(&self, voice: usize) -> f32;
}

/// Keys held in mono and legato, at most this many (the oldest drop out).
const MAX_HELD: usize = 16;

/// The voice mono and legato play.
const MONO_VOICE: usize = 0;

/// Key and pedal state; see the module documentation.
#[derive(Clone, Copy)]
pub(crate) struct Voicing {
    alloc: Allocator,
    /// Sustain pedal down.
    sustain: bool,
    /// Mono and legato: keys held, oldest first, with their velocities.
    held: [(u8, u8); MAX_HELD],
    held_len: usize,
    /// The note started last, for portamento.
    last_note: Option<u8>,
}

impl Voicing {
    pub(crate) const fn new() -> Self {
        Self {
            alloc: Allocator::new(),
            sustain: false,
            held: [(0, 0); MAX_HELD],
            held_len: 0,
            last_note: None,
        }
    }

    /// The voice that started last among those `sounding`.
    pub(crate) fn newest(&self, sounding: impl Fn(usize) -> bool) -> Option<usize> {
        self.alloc.newest(sounding)
    }

    /// Cuts off the sounding voice that started first; `false` if none
    /// sounds. For overload: fewer voices, less to render.
    pub(crate) fn shed_oldest(&mut self, voices: &mut impl Voices) -> bool {
        let Some(v) = self.alloc.oldest(|v| voices.is_sounding(v)) else {
            return false;
        };
        voices.silence(1 << v);
        true
    }

    /// Handles a MIDI message. Omni mode: the channel is ignored. Supported:
    /// notes, the sustain pedal, All Sound Off and All Notes Off; everything
    /// else is ignored.
    pub(crate) fn handle_midi(
        &mut self,
        params: VoicingParams,
        msg: MidiMessage,
        voices: &mut impl Voices,
    ) {
        match msg {
            MidiMessage::NoteOn { note, velocity, .. } => {
                self.note_on(params, note, velocity, voices)
            }
            MidiMessage::NoteOff { note, .. } => self.note_off(params, note, voices),
            MidiMessage::ControlChange {
                controller: CC_SUSTAIN,
                value,
                ..
            } => self.set_sustain(value >= SWITCH_ON, voices),
            MidiMessage::ControlChange {
                controller: CC_ALL_SOUND_OFF,
                ..
            } => {
                self.all_notes_off(voices);
                voices.silence(u32::MAX >> (32 - MAX_VOICES));
            }
            MidiMessage::ControlChange {
                controller: CC_ALL_NOTES_OFF..=DATA_MAX,
                ..
            } => self.all_notes_off(voices),
            MidiMessage::PolyPressure { .. }
            | MidiMessage::ControlChange { .. }
            | MidiMessage::ProgramChange { .. }
            | MidiMessage::ChannelPressure { .. }
            | MidiMessage::PitchBend { .. } => {}
        }
    }

    pub(crate) fn note_on(
        &mut self,
        params: VoicingParams,
        note: u8,
        velocity: u8,
        voices: &mut impl Voices,
    ) {
        let note = note.min(DATA_MAX);
        let glide = params.portamento > 0.0;
        match params.mode {
            VoiceMode::Poly => {
                let idle = core::array::from_fn(|v| !voices.is_sounding(v));
                let unison = params.unison.clamp(1, MAX_UNISON);
                let mut allocations = [Allocation::Fresh(0); MAX_UNISON];
                let found = self
                    .alloc
                    .note_on_group(note, &idle, &mut allocations[..unison]);
                let from = if glide {
                    self.last_note.map(f32::from)
                } else {
                    None
                };
                for (index, &allocation) in allocations[..found].iter().enumerate() {
                    let layer = Layer {
                        index: index as u8,
                        count: unison as u8,
                    };
                    voices.start(allocation, note, velocity, from, layer);
                }
                self.last_note = Some(note);
            }
            VoiceMode::Mono | VoiceMode::Legato => {
                let overlap = self.held_len > 0;
                self.hold(note, velocity);
                self.mono_play(params, note, velocity, overlap, voices);
            }
        }
    }

    pub(crate) fn note_off(&mut self, params: VoicingParams, note: u8, voices: &mut impl Voices) {
        let note = note.min(DATA_MAX);
        // Unheld in every mode: a key pressed in mono and released after a
        // switch to poly must not stay on the stack.
        let current = self.held_len > 0 && self.held[self.held_len - 1].0 == note;
        self.unhold(note);
        if params.mode != VoiceMode::Poly && current && self.held_len > 0 {
            // Back to the newest key still held.
            let (top, velocity) = self.held[self.held_len - 1];
            self.mono_play(params, top, velocity, true, voices);
            return;
        }
        // Releases the poly voices of `note`, and the mono voice when it
        // played `note` and no other key is held (or keeps it for the pedal).
        let released = self.alloc.note_off(note, self.sustain);
        voices.release(released);
    }

    /// All Notes Off (and All Sound Off, together with silencing the
    /// voices): forgets every held key and releases every gated or
    /// sustained voice, the pedal included.
    pub(crate) fn all_notes_off(&mut self, voices: &mut impl Voices) {
        self.held_len = 0;
        self.sustain = false;
        let released = self.alloc.release_all();
        voices.release(released);
    }

    /// Sustain pedal: while down, released keys keep sounding until it is
    /// lifted.
    pub(crate) fn set_sustain(&mut self, down: bool, voices: &mut impl Voices) {
        self.sustain = down;
        if !down {
            let released = self.alloc.release_sustained();
            voices.release(released);
        }
    }

    /// Mono or legato: plays `note` on the mono voices (0, and with unison
    /// the next ones). `overlap`: another key was held already (legato
    /// glides instead of restarting).
    fn mono_play(
        &mut self,
        params: VoicingParams,
        note: u8,
        velocity: u8,
        overlap: bool,
        voices: &mut impl Voices,
    ) {
        let glide = params.portamento > 0.0;
        let unison = params.unison.clamp(1, MAX_UNISON);
        let sounding = voices.is_sounding(MONO_VOICE);
        if params.mode == VoiceMode::Legato && overlap && sounding {
            for index in 0..unison {
                let v = MONO_VOICE + index;
                if voices.is_sounding(v) {
                    voices.legato(v, note, glide);
                } else {
                    // Unison was raised while the key was held: the new voice
                    // joins at the current pitch.
                    let from = glide.then(|| voices.pitch(MONO_VOICE));
                    voices.start(
                        Allocation::Fresh(v),
                        note,
                        velocity,
                        from,
                        Layer {
                            index: index as u8,
                            count: unison as u8,
                        },
                    );
                }
            }
        } else {
            // Mono glides from wherever the voice was, legato only between
            // overlapping keys (handled above).
            let from = (glide && params.mode == VoiceMode::Mono && self.last_note.is_some())
                .then(|| voices.pitch(MONO_VOICE));
            for index in 0..unison {
                let v = MONO_VOICE + index;
                let allocation = if voices.is_sounding(v) {
                    Allocation::Retrigger(v)
                } else {
                    Allocation::Fresh(v)
                };
                voices.start(
                    allocation,
                    note,
                    velocity,
                    from,
                    Layer {
                        index: index as u8,
                        count: unison as u8,
                    },
                );
            }
        }
        (MONO_VOICE..MONO_VOICE + unison).for_each(|v| self.alloc.assign(v, note));
        self.last_note = Some(note);
    }

    /// Adds `note` as the newest held key (moving it there if held already).
    fn hold(&mut self, note: u8, velocity: u8) {
        self.unhold(note);
        if self.held_len == MAX_HELD {
            self.held.copy_within(1.., 0);
            self.held_len -= 1;
        }
        self.held[self.held_len] = (note, velocity);
        self.held_len += 1;
    }

    fn unhold(&mut self, note: u8) {
        if let Some(i) = self.held[..self.held_len]
            .iter()
            .position(|&(n, _)| n == note)
        {
            self.held.copy_within(i + 1..self.held_len, i);
            self.held_len -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what the voicing does.
    #[derive(Default)]
    struct Log {
        sounding: [bool; MAX_VOICES],
        pitch: [f32; MAX_VOICES],
        events: Vec<String>,
    }

    impl Voices for Log {
        fn is_sounding(&self, voice: usize) -> bool {
            self.sounding[voice]
        }
        fn start(
            &mut self,
            allocation: Allocation,
            note: u8,
            _velocity: u8,
            glide_from: Option<f32>,
            layer: Layer,
        ) {
            let (kind, v) = match allocation {
                Allocation::Retrigger(v) => ("retrigger", v),
                Allocation::Fresh(v) => ("fresh", v),
                Allocation::Steal(v) => ("steal", v),
            };
            self.sounding[v] = true;
            self.pitch[v] = note as f32;
            let unison = if layer.count > 1 {
                format!(" ({}/{})", layer.index, layer.count)
            } else {
                String::new()
            };
            self.events.push(match glide_from {
                Some(from) => format!("{kind} {v} {note} from {from}{unison}"),
                None => format!("{kind} {v} {note}{unison}"),
            });
        }
        fn legato(&mut self, voice: usize, note: u8, glide: bool) {
            self.pitch[voice] = note as f32;
            self.events.push(format!(
                "legato {voice} {note}{}",
                if glide { " glide" } else { "" }
            ));
        }
        fn release(&mut self, voices: u32) {
            if voices != 0 {
                self.events.push(format!("release {voices:b}"));
            }
        }
        fn silence(&mut self, voices: u32) {
            self.events.push(format!("silence {voices:b}"));
        }
        fn pitch(&self, voice: usize) -> f32 {
            self.pitch[voice]
        }
    }

    fn params(mode: VoiceMode, portamento: f32) -> VoicingParams {
        VoicingParams {
            mode,
            portamento,
            unison: 1,
        }
    }

    #[test]
    fn unison_starts_a_group_per_key() {
        let poly = VoicingParams {
            unison: 3,
            ..params(VoiceMode::Poly, 0.0)
        };
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.note_on(poly, 60, 100, &mut log);
        v.note_off(poly, 60, &mut log);
        let legato = VoicingParams {
            unison: 2,
            ..params(VoiceMode::Legato, 0.1)
        };
        v.note_on(legato, 64, 100, &mut log);
        v.note_on(legato, 67, 100, &mut log);
        assert_eq!(
            log.events,
            [
                "fresh 0 60 (0/3)",
                "fresh 1 60 (1/3)",
                "fresh 2 60 (2/3)",
                "release 111",
                // The log never silences voices, so mono retriggers them.
                "retrigger 0 64 (0/2)",
                "retrigger 1 64 (1/2)",
                "legato 0 67 glide",
                "legato 1 67 glide",
            ]
        );
        assert_eq!(Layer { index: 0, count: 3 }.position(), -1.0);
        assert_eq!(Layer { index: 1, count: 3 }.position(), 0.0);
        assert_eq!(Layer { index: 2, count: 3 }.position(), 1.0);
        assert_eq!(Layer::SINGLE.position(), 0.0);
    }

    #[test]
    fn legato_restarts_only_without_held_keys_and_returns_to_held_ones() {
        let p = params(VoiceMode::Legato, 0.1);
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.note_on(p, 60, 100, &mut log);
        v.note_on(p, 64, 100, &mut log);
        v.note_on(p, 67, 100, &mut log);
        v.note_off(p, 67, &mut log);
        v.note_off(p, 60, &mut log);
        v.note_off(p, 64, &mut log);
        v.note_on(p, 62, 100, &mut log);
        assert_eq!(
            log.events,
            [
                "fresh 0 60",
                "legato 0 64 glide",
                "legato 0 67 glide",
                // Back to the newest held key; releasing a key that is not
                // playing changes nothing.
                "legato 0 64 glide",
                "release 1",
                // Not overlapping: restarts, without a glide.
                "retrigger 0 62",
            ]
        );
    }

    #[test]
    fn mono_restarts_every_key_and_always_glides() {
        let p = params(VoiceMode::Mono, 0.1);
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.note_on(p, 60, 100, &mut log);
        v.note_on(p, 64, 100, &mut log);
        v.note_off(p, 64, &mut log);
        v.note_off(p, 60, &mut log);
        v.note_on(p, 67, 100, &mut log);
        assert_eq!(
            log.events,
            [
                "fresh 0 60",
                "retrigger 0 64 from 60",
                "retrigger 0 60 from 64",
                "release 1",
                "retrigger 0 67 from 60",
            ]
        );
    }

    #[test]
    fn mono_without_portamento_jumps_and_the_pedal_holds_the_voice() {
        let p = params(VoiceMode::Mono, 0.0);
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.set_sustain(true, &mut log);
        v.note_on(p, 60, 100, &mut log);
        v.note_off(p, 60, &mut log);
        v.set_sustain(false, &mut log);
        assert_eq!(log.events, ["fresh 0 60", "release 1"]);
    }

    #[test]
    fn poly_glides_from_the_last_note() {
        let p = params(VoiceMode::Poly, 0.2);
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.note_on(p, 60, 100, &mut log);
        v.note_on(p, 67, 100, &mut log);
        v.note_off(p, 60, &mut log);
        assert_eq!(
            log.events,
            ["fresh 0 60", "fresh 1 67 from 60", "release 1"]
        );
    }

    #[test]
    fn keys_released_in_poly_leave_the_mono_stack() {
        let (legato, poly) = (params(VoiceMode::Legato, 0.0), params(VoiceMode::Poly, 0.0));
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.note_on(legato, 60, 100, &mut log);
        v.note_off(poly, 60, &mut log);
        log.sounding[0] = false;
        v.note_on(legato, 62, 100, &mut log);
        v.note_off(legato, 62, &mut log);
        // 62 restarts (nothing else is held) and its release does not fall
        // back to the long released 60.
        assert_eq!(
            log.events,
            ["fresh 0 60", "release 1", "fresh 0 62", "release 1"]
        );
    }

    #[test]
    fn legato_starts_voices_added_by_raising_unison() {
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.note_on(params(VoiceMode::Legato, 0.0), 60, 100, &mut log);
        let wider = VoicingParams {
            unison: 2,
            ..params(VoiceMode::Legato, 0.0)
        };
        v.note_on(wider, 64, 100, &mut log);
        assert_eq!(
            log.events,
            ["fresh 0 60", "legato 0 64", "fresh 1 64 (1/2)"]
        );
    }

    #[test]
    fn all_notes_off_releases_everything_and_forgets_the_keys() {
        let p = params(VoiceMode::Mono, 0.0);
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.set_sustain(true, &mut log);
        v.note_on(p, 60, 100, &mut log);
        v.note_on(p, 64, 100, &mut log);
        v.all_notes_off(&mut log);
        log.sounding[0] = false;
        // Nothing held and the pedal is up: releasing a key does nothing.
        v.note_off(p, 60, &mut log);
        assert_eq!(log.events, ["fresh 0 60", "retrigger 0 64", "release 1"]);
    }

    #[test]
    fn switching_from_poly_to_mono_releases_the_old_voices() {
        let (mut v, mut log) = (Voicing::new(), Log::default());
        v.note_on(params(VoiceMode::Poly, 0.0), 60, 100, &mut log);
        v.note_on(params(VoiceMode::Poly, 0.0), 64, 100, &mut log);
        let mono = params(VoiceMode::Mono, 0.0);
        v.note_off(mono, 64, &mut log);
        v.note_off(mono, 60, &mut log);
        assert_eq!(
            log.events,
            ["fresh 0 60", "fresh 1 64", "release 10", "release 1"]
        );
    }
}
