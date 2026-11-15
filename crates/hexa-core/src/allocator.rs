//! Voice allocation: which voice plays a new note.

use crate::MAX_VOICES;

const _: () = assert!(MAX_VOICES <= 32, "voice masks are u32");

/// How a voice was obtained for a note-on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Allocation {
    /// The same note is still sounding on this voice; keep phase and level.
    Retrigger(usize),
    /// The voice was silent.
    Fresh(usize),
    /// The voice was still sounding a different note and gets cut off.
    Steal(usize),
}

impl Allocation {
    /// The voice, however it was obtained.
    pub(crate) fn voice(self) -> usize {
        let (Allocation::Retrigger(v) | Allocation::Fresh(v) | Allocation::Steal(v)) = self;
        v
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Allocator {
    note: [u8; MAX_VOICES],
    /// Key held down.
    gate: [bool; MAX_VOICES],
    /// Key released while the sustain pedal was down; still sounding.
    sustained: [bool; MAX_VOICES],
    /// Note-on order, larger is newer.
    started: [u32; MAX_VOICES],
    counter: u32,
}

impl Allocator {
    pub(crate) const fn new() -> Self {
        Self {
            note: [0; MAX_VOICES],
            gate: [false; MAX_VOICES],
            sustained: [false; MAX_VOICES],
            started: [0; MAX_VOICES],
            counter: 0,
        }
    }

    /// The voice that started last among those `sounding`.
    pub(crate) fn newest(&self, sounding: impl Fn(usize) -> bool) -> Option<usize> {
        (0..MAX_VOICES)
            .filter(|&v| sounding(v))
            .max_by_key(|&v| self.started[v])
    }

    /// The voice that started first among those `sounding`.
    pub(crate) fn oldest(&self, sounding: impl Fn(usize) -> bool) -> Option<usize> {
        (0..MAX_VOICES)
            .filter(|&v| sounding(v))
            .min_by_key(|&v| self.started[v])
    }

    /// Records that `voice` now plays `note` with its key held (mono and
    /// legato pick the voice themselves).
    pub(crate) fn assign(&mut self, voice: usize, note: u8) {
        self.counter = self.counter.wrapping_add(1);
        self.note[voice] = note;
        self.gate[voice] = true;
        self.sustained[voice] = false;
        self.started[voice] = self.counter;
    }

    /// Picks a voice for `note`. `idle` tells which voices are silent.
    ///
    /// Priority: same note still sounding, then a silent voice, then the
    /// oldest released voice, then the oldest sustained voice, then the
    /// oldest held voice.
    #[cfg(test)]
    pub(crate) fn note_on(&mut self, note: u8, idle: &[bool; MAX_VOICES]) -> Allocation {
        let mut out = [Allocation::Fresh(0)];
        self.note_on_group(note, idle, &mut out);
        out[0]
    }

    /// Picks `out.len()` different voices for `note` (unison), each by the
    /// priority of [`Self::note_on`]: the voices still sounding `note` are
    /// retriggered first. Returns how many were found (fewer only if there
    /// are not enough voices).
    pub(crate) fn note_on_group(
        &mut self,
        note: u8,
        idle: &[bool; MAX_VOICES],
        out: &mut [Allocation],
    ) -> usize {
        let mut taken = 0u32;
        let mut count = 0;
        for (v, &silent) in idle.iter().enumerate() {
            if count < out.len() && !silent && self.note[v] == note {
                out[count] = Allocation::Retrigger(v);
                taken |= 1 << v;
                count += 1;
            }
        }
        // In the order they started, which is their place in the group:
        // each keeps its detune and pan.
        out[..count].sort_unstable_by_key(|a| self.started[a.voice()]);
        while count < out.len() {
            let free = |v: usize| taken & (1 << v) == 0;
            let oldest = |pred: &dyn Fn(usize) -> bool| {
                (0..MAX_VOICES)
                    .filter(|&v| free(v) && pred(v))
                    .min_by_key(|&v| self.started[v])
            };
            let allocation = if let Some(v) = (0..MAX_VOICES).find(|&v| free(v) && idle[v]) {
                Allocation::Fresh(v)
            } else if let Some(v) = oldest(&|v| !self.gate[v] && !self.sustained[v]) {
                Allocation::Steal(v)
            } else if let Some(v) = oldest(&|v| self.sustained[v]) {
                Allocation::Steal(v)
            } else if let Some(v) = oldest(&|_| true) {
                Allocation::Steal(v)
            } else {
                break;
            };
            let v = allocation.voice();
            out[count] = allocation;
            taken |= 1 << v;
            count += 1;
        }
        for allocation in &out[..count] {
            self.assign(allocation.voice(), note);
        }
        count
    }

    /// Releases every held voice playing `note` and returns them as a bit
    /// mask. With `sustain` (pedal down) they are kept sounding instead and
    /// the mask is empty; [`Self::release_sustained`] releases them later.
    pub(crate) fn note_off(&mut self, note: u8, sustain: bool) -> u32 {
        let mut released = 0;
        for v in 0..MAX_VOICES {
            if self.gate[v] && self.note[v] == note {
                self.gate[v] = false;
                if sustain {
                    self.sustained[v] = true;
                } else {
                    released |= 1 << v;
                }
            }
        }
        released
    }

    /// Releases every held and sustained voice and returns them as a bit mask.
    pub(crate) fn release_all(&mut self) -> u32 {
        let mut released = 0;
        for v in 0..MAX_VOICES {
            if self.gate[v] || self.sustained[v] {
                self.gate[v] = false;
                self.sustained[v] = false;
                released |= 1 << v;
            }
        }
        released
    }

    /// Releases all sustained voices (pedal up) and returns them as a bit mask.
    pub(crate) fn release_sustained(&mut self) -> u32 {
        let mut released = 0;
        for v in 0..MAX_VOICES {
            if self.sustained[v] {
                self.sustained[v] = false;
                released |= 1 << v;
            }
        }
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_then_retrigger() {
        let mut a = Allocator::new();
        let mut idle = [true; MAX_VOICES];
        assert_eq!(a.note_on(60, &idle), Allocation::Fresh(0));
        idle[0] = false;
        assert_eq!(a.note_on(62, &idle), Allocation::Fresh(1));
        idle[1] = false;
        assert_eq!(a.note_on(60, &idle), Allocation::Retrigger(0));
    }

    #[test]
    fn released_voice_is_stolen_before_held_ones() {
        let mut a = Allocator::new();
        let mut idle = [true; MAX_VOICES];
        for n in 0..MAX_VOICES as u8 {
            assert_eq!(a.note_on(n, &idle), Allocation::Fresh(n as usize));
            idle[n as usize] = false;
        }
        // Release a voice in the middle; it is not the oldest overall.
        assert_eq!(a.note_off(5, false), 1 << 5);
        assert_eq!(a.note_on(100, &idle), Allocation::Steal(5));
        // All held now: the oldest (note 0 on voice 0) gets stolen.
        assert_eq!(a.note_on(101, &idle), Allocation::Steal(0));
        assert_eq!(a.note[0], 101);
    }

    #[test]
    fn sustain_keeps_voices_until_pedal_up() {
        let mut a = Allocator::new();
        let mut idle = [true; MAX_VOICES];
        assert_eq!(a.note_on(60, &idle), Allocation::Fresh(0));
        idle[0] = false;
        assert_eq!(a.note_on(62, &idle), Allocation::Fresh(1));
        idle[1] = false;
        assert_eq!(a.note_off(60, true), 0);
        // Striking a sustained note again takes it back; it is held now.
        assert_eq!(a.note_on(60, &idle), Allocation::Retrigger(0));
        assert_eq!(a.note_off(62, true), 0);
        assert_eq!(a.release_sustained(), 1 << 1);
        assert_eq!(a.release_sustained(), 0);
        assert_eq!(a.note_off(60, false), 1 << 0);
    }

    #[test]
    fn sustained_voices_are_stolen_after_released_before_held() {
        let mut a = Allocator::new();
        let mut idle = [true; MAX_VOICES];
        for n in 0..MAX_VOICES as u8 {
            a.note_on(n, &idle);
            idle[n as usize] = false;
        }
        assert_eq!(a.note_off(3, true), 0);
        assert_eq!(a.note_off(7, false), 1 << 7);
        assert_eq!(a.note_on(100, &idle), Allocation::Steal(7));
        assert_eq!(a.note_on(101, &idle), Allocation::Steal(3));
        assert_eq!(a.note_on(102, &idle), Allocation::Steal(0));
    }

    #[test]
    fn groups_take_different_voices_and_retrigger_their_own() {
        let mut a = Allocator::new();
        let mut idle = [true; MAX_VOICES];
        let mut out = [Allocation::Fresh(0); 3];
        assert_eq!(a.note_on_group(60, &idle, &mut out), 3);
        assert_eq!(
            out,
            [
                Allocation::Fresh(0),
                Allocation::Fresh(1),
                Allocation::Fresh(2)
            ]
        );
        (0..3).for_each(|v| idle[v] = false);
        assert_eq!(a.note_on_group(64, &idle, &mut out), 3);
        assert_eq!(
            out,
            [
                Allocation::Fresh(3),
                Allocation::Fresh(4),
                Allocation::Fresh(5)
            ]
        );
        (3..6).for_each(|v| idle[v] = false);
        // The same note again: its own three voices.
        assert_eq!(a.note_on_group(60, &idle, &mut out), 3);
        assert_eq!(
            out,
            [
                Allocation::Retrigger(0),
                Allocation::Retrigger(1),
                Allocation::Retrigger(2)
            ]
        );
        // Releasing frees the whole group.
        assert_eq!(a.note_off(60, false), 0b111);
    }

    #[test]
    fn retriggered_groups_keep_their_order() {
        // A group of 60 that started on voices 9, 2 and 5, in that order.
        let mut out = [Allocation::Fresh(0); 3];
        let mut a = Allocator::new();
        a.note[9] = 60;
        a.note[2] = 60;
        a.note[5] = 60;
        (a.started[9], a.started[2], a.started[5]) = (1, 2, 3);
        a.counter = 3;
        let idle = core::array::from_fn(|v| ![2, 5, 9].contains(&v));
        assert_eq!(a.note_on_group(60, &idle, &mut out), 3);
        assert_eq!(
            out,
            [
                Allocation::Retrigger(9),
                Allocation::Retrigger(2),
                Allocation::Retrigger(5)
            ]
        );
    }

    #[test]
    fn release_all_frees_held_and_sustained() {
        let mut a = Allocator::new();
        let mut idle = [true; MAX_VOICES];
        a.note_on(60, &idle);
        idle[0] = false;
        a.note_on(62, &idle);
        assert_eq!(a.note_off(60, true), 0);
        assert_eq!(a.release_all(), 0b11);
        assert_eq!(a.release_all(), 0);
        assert_eq!(a.release_sustained(), 0);
    }

    #[test]
    fn note_off_ignores_other_notes_and_released_voices() {
        let mut a = Allocator::new();
        a.note_on(60, &[true; MAX_VOICES]);
        assert_eq!(a.note_off(61, false), 0);
        assert_eq!(a.note_off(60, false), 1);
        assert_eq!(a.note_off(60, false), 0);
    }
}
