//! Sequencer runtime of one track (ARCHITECTURE §6.4 step 4, §6.5).
//!
//! Per processor call, the instrument's events come from:
//! 1. note-offs of notes flushed by stop, locate, loop wrap or panic (offset 0),
//! 2. the program's notes starting in the call (probability rolled per track) merged in time order
//!    with live input (MIDI from monitored inputs, `LiveNote`),
//! 3. note-offs of notes whose end falls into the call.
//!
//! **Note chasing** (like Ableton Live's "chase MIDI notes"): after a playback start, a locate while
//! playing and a loop wrap, the first call also starts the notes that began before the position
//! and still sound there (pads, long bass notes), at offset 0 with their own end. Probability
//! rolls for them as for any note. Program swaps do not chase: an edit under the playhead does not
//! start a note in the middle.
//!
//! Every note-on goes into the track's **active-notes table** together with the instrument slot
//! that received it; its note-off comes only from the table. The table belongs to the track, not
//! to the program, so a program swap neither cuts nor hangs a note. One entry per channel and key:
//! a new note-on for a sounding key first ends the old note (retrigger), so every note-on gets
//! exactly one note-off.

use klang_dsp::rng::Rng;
use komponist_model::{TempoMap, Tick, TrackId};

use crate::note_schedule::{NoteView, RepeatScratch};
use crate::processor::{Event, EventKind, NOTE_ID_NONE};
use crate::program::{PROBABILITY_FOLLOW, SeqNote, SlotId};

/// Notes one track holds at once (sequencer and live); further note-ons are dropped and counted.
pub(crate) const MAX_ACTIVE_NOTES: usize = 256;

/// Events for one processor call, sorted by offset when finished.
///
/// Sorting keeps the push order within an offset, except that note-ons go after everything else
/// at their offset (note-offs before note-ons, `Processor::process`). No allocation: both buffers
/// are preallocated and `sort_unstable` works in place (the monotonic push counter makes the keys
/// unique, so the order is stable).
#[derive(Debug)]
pub(crate) struct EventList {
    keyed: Vec<(u64, Event)>,
    sorted: Vec<Event>,
    capacity: usize,
    note_off_first: bool,
    /// Sequence number of the next push; reset by `clear`, never reused after `remove_note_on`.
    /// A block pushes far fewer than 2^31 events, so the 31 bits of the key cannot wrap.
    next_seq: u32,
}

impl EventList {
    /// Non-RT. A list for at most `capacity` events.
    pub(crate) fn new(capacity: usize) -> EventList {
        Self::with_note_off_first(capacity, true)
    }

    pub(crate) fn with_note_off_first(capacity: usize, note_off_first: bool) -> EventList {
        EventList {
            keyed: Vec::with_capacity(capacity),
            sorted: Vec::with_capacity(capacity),
            capacity,
            note_off_first,
            next_seq: 0,
        }
    }

    /// RT. Empties the list.
    pub(crate) fn clear(&mut self) {
        self.keyed.clear();
        self.sorted.clear();
        self.next_seq = 0;
    }

    /// Free places.
    pub(crate) fn room(&self) -> usize {
        self.capacity - self.keyed.len()
    }

    /// RT. Adds an event; `false` when the list is full.
    pub(crate) fn push(&mut self, offset: u32, kind: EventKind) -> bool {
        if self.room() == 0 {
            return false;
        }
        let rank = u64::from(self.note_off_first && matches!(kind, EventKind::NoteOn { .. }));
        let seq = u64::from(self.next_seq & 0x7fff_ffff);
        self.next_seq = self.next_seq.wrapping_add(1);
        let key = u64::from(offset) << 32 | rank << 31 | seq;
        self.keyed.push((key, Event { offset, kind }));
        true
    }

    /// Offset of the pending note-on with `note_id`, if it is in the list.
    pub(crate) fn note_on_offset(&self, note_id: u32) -> Option<u32> {
        self.keyed.iter().find_map(|(_, e)| match e.kind {
            EventKind::NoteOn { note_id: id, .. } if id == note_id => Some(e.offset),
            _ => None,
        })
    }

    /// RT. Takes the note-on with `note_id` out of the list again.
    pub(crate) fn remove_note_on(&mut self, note_id: u32) {
        let found = self.keyed.iter().position(
            |(_, e)| matches!(e.kind, EventKind::NoteOn { note_id: id, .. } if id == note_id),
        );
        if let Some(index) = found {
            self.keyed.remove(index);
        }
    }

    /// The events as last sorted by [`EventList::finish`].
    pub(crate) fn sorted(&self) -> &[Event] {
        &self.sorted
    }

    /// RT. The events sorted for the processor call.
    pub(crate) fn finish(&mut self) -> &[Event] {
        self.keyed.sort_unstable_by_key(|(key, _)| *key);
        self.sorted.clear();
        self.sorted.extend(self.keyed.iter().map(|(_, e)| *e));
        &self.sorted
    }
}

/// Engine-wide CLAP note ids, never [`NOTE_ID_NONE`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NoteIds(u32);

impl NoteIds {
    /// The next id.
    pub(crate) fn next(&mut self) -> u32 {
        let id = self.0;
        self.0 = self.0.wrapping_add(1);
        if self.0 == NOTE_ID_NONE {
            self.0 = 0;
        }
        id
    }
}

/// The stretch of song samples one processor call covers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window<'a> {
    /// Tempo map of the program.
    pub(crate) map: &'a TempoMap,
    /// Sample rate in Hz.
    pub(crate) rate: f64,
    /// Song sample at offset 0.
    pub(crate) start: i64,
    /// Frames in the call.
    pub(crate) frames: u32,
    /// Transport running: sequencer notes play and end.
    pub(crate) playing: bool,
}

impl Window<'_> {
    fn sample(&self, tick: Tick) -> i64 {
        self.map.tick_to_sample(tick, self.rate)
    }

    fn end(&self) -> i64 {
        self.start + i64::from(self.frames)
    }

    /// First nonnegative tick whose rounded sample is at or after `sample`, even when
    /// several ticks round to the same sample.
    fn first_tick(&self, sample: i64) -> Tick {
        let (mut low, mut high) = (0, i64::MAX);
        if sample <= 0 {
            return Tick::ZERO;
        }
        while low < high {
            let middle = low + (high - low) / 2;
            if self.sample(Tick(middle)) < sample {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Tick(low)
    }
}

/// One live MIDI message for a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveMsg {
    /// Offset in the call.
    pub(crate) offset: u32,
    /// MIDI 1.0 channel message.
    pub(crate) data: [u8; 3],
    /// Reaches the instrument (monitoring, or a `LiveNote`).
    pub(crate) play: bool,
    /// Captured for recording (armed track while recording).
    pub(crate) capture: bool,
}

/// Captured input, reported as `RecordedNoteOn/Off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Captured {
    /// A key went down.
    On {
        /// Offset in the call.
        offset: u32,
        /// MIDI key.
        key: u8,
        /// MIDI velocity 1 ..= 127.
        velocity: u8,
    },
    /// A captured key went up.
    Off {
        /// Offset in the call.
        offset: u32,
        /// MIDI key.
        key: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ActiveNote {
    key: u8,
    channel: u8,
    note_id: u32,
    /// Instrument slot that received the note-on; the note-off goes only there.
    slot: SlotId,
    /// End tick of a sequencer note; `None` for a live note (ends with its note-off).
    end: Option<Tick>,
    /// Song sample of the note-on; the note-off comes at least one sample later.
    on: i64,
    /// Note-off due at offset 0 of the next call (stop, locate, loop wrap, panic).
    flushed: bool,
}

/// The probability seed of a track: the engine seed mixed with the track id, so every track rolls
/// its own reproducible sequence.
pub(crate) fn track_seed(base: u32, id: TrackId) -> u32 {
    let mut x = id.0 ^ (u64::from(base) << 32 | u64::from(base));
    // splitmix64 finaliser.
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    (x ^ (x >> 32)) as u32
}

/// Sequencer state of one track: active notes, probability RNG, open captured keys.
pub(crate) struct TrackSeq {
    active: Vec<ActiveNote>,
    rng: Rng,
    /// Outcome of the last roll per key, for [`PROBABILITY_FOLLOW`].
    last_roll: [bool; 128],
    /// Captured keys still down.
    rec_open: u128,
    /// The next playing call starts the notes that sound across its first frame (note chasing).
    chase: bool,
}

impl TrackSeq {
    /// Non-RT. Empty, with the table preallocated.
    pub(crate) fn new() -> TrackSeq {
        TrackSeq {
            active: Vec::with_capacity(MAX_ACTIVE_NOTES),
            rng: Rng::default(),
            last_roll: [true; 128],
            rec_open: 0,
            chase: false,
        }
    }

    /// RT. The next playing call chases notes: playback start, locate while playing, loop wrap.
    pub(crate) fn chase(&mut self) {
        self.chase = true;
    }

    /// RT. Forgets all notes and captures (the state goes to another track).
    pub(crate) fn clear(&mut self) {
        self.active.clear();
        self.rec_open = 0;
        self.chase = false;
        self.last_roll = [true; 128];
    }

    /// RT. Restarts the probability sequence (playback start).
    pub(crate) fn reseed(&mut self, seed: u32) {
        self.rng = Rng::new(seed);
        self.last_roll = [true; 128];
    }

    /// Notes in the table.
    #[cfg(test)]
    pub(crate) fn sounding(&self) -> usize {
        self.active.len()
    }

    /// RT. Schedules a note-off at offset 0 of the next call for every sequencer note, and for
    /// live notes too when `live_too`.
    pub(crate) fn flush(&mut self, live_too: bool) {
        for n in &mut self.active {
            if live_too || n.end.is_some() {
                n.flushed = true;
            }
        }
    }

    /// RT. Forgets the notes held by `slot` (its processor was replaced or removed), so the
    /// next processor there gets no note-offs for notes it never saw.
    pub(crate) fn forget_slot(&mut self, slot: SlotId) {
        let mut k = 0;
        while k < self.active.len() {
            if self.active[k].slot == slot {
                self.active.swap_remove(k);
            } else {
                k += 1;
            }
        }
    }

    /// RT. Takes every note of `slot` out of the table without a note-off, calling `f(key,
    /// channel, note_id)` for each: the notes the slot's processor holds, whose note-offs the
    /// caller now owns (a MIDI effect stage is put in front of an instrument that is playing).
    pub(crate) fn drain_slot(&mut self, slot: SlotId, mut f: impl FnMut(u8, u8, u32)) {
        let mut k = 0;
        while k < self.active.len() {
            if self.active[k].slot == slot {
                let n = self.active.swap_remove(k);
                f(n.key, n.channel, n.note_id);
            } else {
                k += 1;
            }
        }
    }

    /// RT. Ends every open capture: calls `f(key)` per key still down.
    pub(crate) fn close_captures(&mut self, mut f: impl FnMut(u8)) {
        for key in 0..128u8 {
            if self.rec_open & 1 << key != 0 {
                f(key);
            }
        }
        self.rec_open = 0;
    }

    /// Whether `note` plays: probability 0 never, 100 always, between rolled; follow notes repeat
    /// the last outcome of their key.
    fn roll(&mut self, note: &SeqNote) -> bool {
        let key = usize::from(note.key & 0x7f);
        let plays = match note.probability {
            PROBABILITY_FOLLOW => return self.last_roll[key],
            0 => false,
            p if p >= 100 => true,
            p => self.rng.below(100) < usize::from(p),
        };
        self.last_roll[key] = plays;
        plays
    }

    fn find(&self, channel: u8, key: u8) -> Option<usize> {
        self.active
            .iter()
            .position(|n| n.channel == channel && n.key == key)
    }

    /// Room the event list must keep for note-offs of the table.
    pub(crate) fn reserved(&self) -> usize {
        self.active.len()
    }

    /// RT. Step 1: note-offs at offset 0 for flushed notes (only to the slot that has them).
    pub(crate) fn flushed_offs(&mut self, instrument: Option<SlotId>, out: &mut EventList) {
        let mut k = 0;
        while k < self.active.len() {
            let n = self.active[k];
            if n.flushed {
                if Some(n.slot) == instrument {
                    out.push(0, note_off(&n));
                }
                self.active.swap_remove(k);
            } else {
                k += 1;
            }
        }
    }

    /// RT. Steps 2 and 3 for one call: note-ons from `notes` (if playing) and `live` in time
    /// order, then the note-offs that fall into the call. `instrument` is the slot that plays
    /// (`None`: no instrument, bypassed or failed; notes are rolled but not played). Captured live
    /// input goes to `capture`. Returns the number of dropped events.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sequence(
        &mut self,
        notes: NoteView<'_>,
        live: &[LiveMsg],
        win: &Window<'_>,
        instrument: Option<SlotId>,
        ids: &mut NoteIds,
        repeat_scratch: &mut RepeatScratch,
        out: &mut EventList,
        capture: &mut impl FnMut(Captured),
    ) -> u32 {
        let (start, end) = (win.start, win.end());
        let (lo, hi) = if win.playing {
            let lo = notes.flat.partition_point(|n| win.sample(n.start) < start);
            (
                lo,
                lo + notes.flat[lo..].partition_point(|n| win.sample(n.start) < end),
            )
        } else {
            (0, 0)
        };
        let ticks = if win.playing && notes.has_repeats() {
            win.first_tick(start)..win.first_tick(end)
        } else {
            Tick::ZERO..Tick::ZERO
        };
        let mut dropped = 0;
        if win.playing && core::mem::take(&mut self.chase) {
            let floor = win.map.sample_to_tick(start, win.rate);
            let before = notes.merge(
                0..lo,
                Tick::ZERO..ticks.start,
                Some(floor),
                Some(repeat_scratch),
            );
            dropped += self.chase_notes(before, win, floor, instrument, ids, out);
        }
        let mut scheduled = notes
            .merge(lo..hi, ticks, None, Some(repeat_scratch))
            .peekable();
        let mut j = 0;
        loop {
            let seq_offset = scheduled
                .peek()
                .map(|note| (win.sample(note.start) - start) as u32);
            let live_offset = live.get(j).map(|m| m.offset);
            let take_live = match (seq_offset, live_offset) {
                (None, None) => break,
                (Some(s), Some(l)) => l < s,
                (Some(_), None) => false,
                (None, Some(_)) => true,
            };
            if take_live {
                dropped += self.live(live[j], win, instrument, ids, out, capture);
                j += 1;
                continue;
            }
            let note = scheduled.next().expect("sequenced note was just peeked");
            let offset = seq_offset.unwrap_or(0);
            if self.roll(&note)
                && let Some(slot) = instrument
                && note.key <= 127
            {
                let on = Pending {
                    offset,
                    key: note.key,
                    velocity: note.velocity.clamp(1, 127),
                    channel: note.channel & 0x0f,
                    end: Some(note.start + note.length),
                };
                if !self.note_on(on, slot, win, ids, out) {
                    dropped += 1;
                }
            }
        }
        if win.playing {
            let mut k = 0;
            while k < self.active.len() {
                let n = self.active[k];
                if let Some(off) = self.off_sample(&n, win)
                    && off < end
                {
                    if Some(n.slot) == instrument {
                        out.push((off - start).max(0) as u32, note_off(&n));
                    }
                    self.active.swap_remove(k);
                } else {
                    k += 1;
                }
            }
        }
        dropped
    }

    /// RT. Note chasing: starts, at offset 0, every note of `before` (the notes that start before
    /// the call, sorted by start) that still sounds at the call's first frame. Returns the number
    /// of dropped note-ons.
    ///
    /// Cost: one tick comparison per earlier note of the track (10 000 notes ≈ 10 µs), only in the
    /// first call after a start, locate or wrap.
    fn chase_notes(
        &mut self,
        before: impl Iterator<Item = SeqNote>,
        win: &Window<'_>,
        floor: Tick,
        instrument: Option<SlotId>,
        ids: &mut NoteIds,
        out: &mut EventList,
    ) -> u32 {
        // A note ending at or before this tick cannot sound at `win.start` (tick → sample is
        // monotonic and `floor` maps back to a sample ≤ `win.start`).
        let mut dropped = 0;
        for note in before {
            let end = note.start + note.length;
            if end <= floor || win.sample(end) <= win.start {
                continue;
            }
            if self.roll(&note)
                && let Some(slot) = instrument
                && note.key <= 127
            {
                let on = Pending {
                    offset: 0,
                    key: note.key,
                    velocity: note.velocity.clamp(1, 127),
                    channel: note.channel & 0x0f,
                    end: Some(end),
                };
                if !self.note_on(on, slot, win, ids, out) {
                    dropped += 1;
                }
            }
        }
        dropped
    }

    /// Song sample of a sequencer note's note-off, at least one sample after its note-on.
    fn off_sample(&self, n: &ActiveNote, win: &Window<'_>) -> Option<i64> {
        n.end.map(|end| win.sample(end).max(n.on + 1))
    }

    /// Plays a note-on (ending a sounding note on the same channel and key first). `false` when
    /// the table or the event list is full.
    fn note_on(
        &mut self,
        on: Pending,
        slot: SlotId,
        win: &Window<'_>,
        ids: &mut NoteIds,
        out: &mut EventList,
    ) -> bool {
        let at = win.start + i64::from(on.offset);
        if let Some(k) = self.find(on.channel, on.key) {
            let old = self.active.swap_remove(k);
            if old.slot == slot {
                if out.note_on_offset(old.note_id) == Some(on.offset) {
                    // Both start on the same sample: the old note never sounds (a note-off
                    // there would sort before its own note-on).
                    out.remove_note_on(old.note_id);
                } else {
                    let off = self.off_sample(&old, win).map_or(at, |s| s.min(at));
                    out.push((off - win.start).max(0) as u32, note_off(&old));
                }
            }
        }
        // Keep room for the note-offs of the table, this note's included.
        if self.active.len() >= MAX_ACTIVE_NOTES || out.room() < self.active.len() + 2 {
            return false;
        }
        let note_id = ids.next();
        out.push(
            on.offset,
            EventKind::NoteOn {
                key: on.key,
                velocity: on.velocity,
                channel: on.channel,
                note_id,
            },
        );
        self.active.push(ActiveNote {
            key: on.key,
            channel: on.channel,
            note_id,
            slot,
            end: on.end,
            on: at,
            flushed: false,
        });
        true
    }

    /// One live message: capture and play. Returns the number of dropped events.
    fn live(
        &mut self,
        m: LiveMsg,
        win: &Window<'_>,
        instrument: Option<SlotId>,
        ids: &mut NoteIds,
        out: &mut EventList,
        capture: &mut impl FnMut(Captured),
    ) -> u32 {
        let [status, data1, data2] = m.data;
        let (channel, key, velocity) = (status & 0x0f, data1 & 0x7f, data2 & 0x7f);
        let bit = 1u128 << key;
        match status & 0xf0 {
            0x90 if velocity > 0 => {
                if m.capture {
                    self.rec_open |= bit;
                    capture(Captured::On {
                        offset: m.offset,
                        key,
                        velocity,
                    });
                }
                if m.play
                    && let Some(slot) = instrument
                {
                    let on = Pending {
                        offset: m.offset,
                        key,
                        velocity,
                        channel,
                        end: None,
                    };
                    if !self.note_on(on, slot, win, ids, out) {
                        return 1;
                    }
                }
            }
            0x80 | 0x90 => {
                if m.capture && self.rec_open & bit != 0 {
                    self.rec_open &= !bit;
                    capture(Captured::Off {
                        offset: m.offset,
                        key,
                    });
                }
                if m.play
                    && let Some(k) = self.find(channel, key)
                    && self.active[k].end.is_none()
                {
                    let n = self.active[k];
                    if out.note_on_offset(n.note_id) == Some(m.offset) {
                        // Released on the sample it started: the note-off would sort before the
                        // note-on, so it goes out at the start of the next call.
                        self.active[k].flushed = true;
                    } else {
                        self.active.swap_remove(k);
                        if Some(n.slot) == instrument {
                            out.push(m.offset, note_off(&n));
                        }
                    }
                }
            }
            0xa0..=0xe0 if m.play && instrument.is_some() => {
                if out.room() <= self.active.len() + 1 {
                    return 1;
                }
                out.push(m.offset, EventKind::Midi(m.data));
            }
            _ => {}
        }
        0
    }
}

/// A note-on about to be played.
#[derive(Debug, Clone, Copy)]
struct Pending {
    offset: u32,
    key: u8,
    velocity: u8,
    channel: u8,
    end: Option<Tick>,
}

fn note_off(n: &ActiveNote) -> EventKind {
    EventKind::NoteOff {
        key: n.key,
        channel: n.channel,
        note_id: n.note_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{Meter, Tempo};

    const SLOT: SlotId = SlotId(3);

    fn map() -> TempoMap {
        // 120 bpm at 48 kHz: 25 samples per tick.
        TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR)
    }

    fn note(start: i64, length: i64, key: u8) -> SeqNote {
        SeqNote {
            start: Tick(start),
            length: Tick(length),
            key,
            velocity: 100,
            probability: 100,
            channel: 0,
        }
    }

    struct Run {
        seq: TrackSeq,
        ids: NoteIds,
        repeat_scratch: RepeatScratch,
        out: EventList,
        map: TempoMap,
        pos: i64,
        captured: Vec<(i64, Captured)>,
    }

    impl Run {
        fn new() -> Run {
            Run {
                seq: TrackSeq::new(),
                ids: NoteIds::default(),
                repeat_scratch: RepeatScratch::new(),
                out: EventList::new(crate::MAX_EVENTS_PER_BLOCK),
                map: map(),
                pos: 0,
                captured: Vec::with_capacity(64),
            }
        }

        /// One call of `frames`: `(absolute sample, event)` pairs.
        fn call(
            &mut self,
            notes: &[SeqNote],
            live: &[LiveMsg],
            frames: u32,
            instrument: Option<SlotId>,
        ) -> Vec<(i64, EventKind)> {
            self.call_view(NoteView::from_slice(notes), live, frames, instrument)
        }

        fn call_view(
            &mut self,
            notes: NoteView<'_>,
            live: &[LiveMsg],
            frames: u32,
            instrument: Option<SlotId>,
        ) -> Vec<(i64, EventKind)> {
            let win = Window {
                map: &self.map,
                rate: 48_000.0,
                start: self.pos,
                frames,
                playing: true,
            };
            let pos = self.pos;
            let Run {
                seq,
                ids,
                repeat_scratch,
                out,
                captured,
                ..
            } = self;
            // The RT part without allocation (`captured` has room).
            let events = assert_no_alloc::assert_no_alloc(|| {
                out.clear();
                seq.flushed_offs(instrument, out);
                seq.sequence(
                    notes,
                    live,
                    &win,
                    instrument,
                    ids,
                    repeat_scratch,
                    out,
                    &mut |c| captured.push((pos, c)),
                );
                out.finish()
            });
            let events = events
                .iter()
                .map(|e| (pos + i64::from(e.offset), e.kind))
                .collect();
            self.pos += i64::from(frames);
            events
        }
    }

    #[test]
    fn a_seek_near_the_end_of_a_million_repeats_only_sees_the_current_periods() {
        use crate::note_schedule::{NoteSchedule, RepeatedMidi};

        let end = 1_000_000;
        let repeated = NoteSchedule::compile(
            Vec::new(),
            vec![RepeatedMidi::new(
                Tick(0),
                Tick(end),
                Tick(1),
                0,
                vec![note(0, 1, 61)],
            )],
        );
        let expanded_tail = (end - 5..end)
            .map(|start| note(start, 1, 61))
            .collect::<Vec<_>>();
        let mut compressed = Run::new();
        let mut reference = Run::new();
        for run in [&mut compressed, &mut reference] {
            run.pos = 25 * (end - 4) + 12;
            run.seq.chase();
        }
        let actual = compressed.call_view(repeated.view(), &[], 128, Some(SLOT));
        assert_eq!(actual, reference.call(&expanded_tail, &[], 128, Some(SLOT)));
        assert_eq!(
            keys(&actual)
                .iter()
                .filter(|(_, kind, _)| *kind == "on")
                .count(),
            4
        );
    }

    #[test]
    fn compact_loops_match_expanded_overlap_chase_and_partial_period() {
        use crate::note_schedule::{NoteSchedule, RepeatedMidi};

        let velocity = |note: SeqNote, velocity| SeqNote { velocity, ..note };
        let first = vec![
            velocity(note(0, 6, 60), 40),
            velocity(note(0, 2, 60), 50),
            note(3, 7, 64),
        ];
        let second = vec![note(0, 3, 59), velocity(note(0, 5, 60), 90), note(4, 4, 62)];
        let regular = note(10, 12, 60);
        let compact = NoteSchedule::compile(
            vec![(regular, 1)],
            vec![
                RepeatedMidi::new(Tick(0), Tick(27), Tick(10), 0, first.clone()),
                RepeatedMidi::new(Tick(2), Tick(25), Tick(8), 2, second.clone()),
            ],
        );
        let expand = |base: i64, end: i64, period: usize, notes: Vec<SeqNote>| {
            (base..end).step_by(period).flat_map(move |offset| {
                let notes = notes.clone();
                notes.into_iter().filter_map(move |mut note| {
                    note.start.0 += offset;
                    note.length.0 = note.length.0.min(end - note.start.0);
                    (note.length.0 > 0).then_some(note)
                })
            })
        };
        let mut expanded: Vec<_> = expand(0, 27, 10, first)
            .chain([regular])
            .chain(expand(2, 27, 8, second))
            .collect();
        expanded.sort_by_key(|note| (note.start, note.key));
        assert_eq!(compact.iter().collect::<Vec<_>>(), expanded);
        assert_eq!(compact.len(), expanded.len());

        for start_sample in [0, 249, 250, 273, 649, 650] {
            let mut compressed = Run::new();
            let mut reference = Run::new();
            for run in [&mut compressed, &mut reference] {
                run.pos = start_sample;
                run.seq.chase();
            }
            for _ in 0..10 {
                assert_eq!(
                    compressed.call_view(compact.view(), &[], 73, Some(SLOT)),
                    reference.call(&expanded, &[], 73, Some(SLOT)),
                    "started at sample {start_sample}",
                );
            }
        }

        let trimmed = compact.trimmed(Tick(25)).unwrap();
        expanded.retain(|note| note.start.0 < 25);
        for note in &mut expanded {
            note.length.0 = note.length.0.min(25 - note.start.0);
        }
        assert_eq!(trimmed.iter().collect::<Vec<_>>(), expanded);
        assert_eq!(trimmed.stored_notes(), compact.stored_notes());
    }

    fn keys(events: &[(i64, EventKind)]) -> Vec<(i64, &'static str, u8)> {
        events
            .iter()
            .map(|(s, k)| match *k {
                EventKind::NoteOn { key, .. } => (*s, "on", key),
                EventKind::NoteOff { key, .. } => (*s, "off", key),
                EventKind::AllNotesOff => (*s, "all", 0),
                EventKind::Midi(m) => (*s, "midi", m[1]),
                EventKind::Param { id, .. } => (*s, "param", id as u8),
            })
            .collect()
    }

    #[test]
    fn event_list_sorts_stably_with_note_offs_first() {
        let mut list = EventList::new(8);
        let on = |key| EventKind::NoteOn {
            key,
            velocity: 1,
            channel: 0,
            note_id: 0,
        };
        let off = |key| EventKind::NoteOff {
            key,
            channel: 0,
            note_id: 0,
        };
        list.push(5, on(1));
        list.push(5, off(2));
        list.push(0, EventKind::Midi([0xb0, 1, 2]));
        list.push(5, off(3));
        list.push(5, on(4));
        list.push(0, EventKind::AllNotesOff);
        let got: Vec<_> = list.finish().iter().map(|e| (e.offset, e.kind)).collect();
        assert_eq!(
            got,
            [
                (0, EventKind::Midi([0xb0, 1, 2])),
                (0, EventKind::AllNotesOff),
                (5, off(2)),
                (5, off(3)),
                (5, on(1)),
                (5, on(4)),
            ]
        );
        assert_eq!(list.room(), 2);
        assert!(list.push(1, off(1)) && list.push(1, off(1)));
        assert!(!list.push(1, off(1)));
    }

    /// After `remove_note_on` the list shrinks; a new event must still sort after everything
    /// pushed before it (the old `keyed.len()` key reused a number and broke the send order).
    #[test]
    fn retriggered_note_keeps_send_order_after_remove() {
        let mut list = EventList::new(8);
        let on = |note_id| EventKind::NoteOn {
            key: 60,
            velocity: 1,
            channel: 0,
            note_id,
        };
        let cc = |v| EventKind::Midi([0xb0, 64, v]);
        list.push(7, cc(1));
        list.push(7, on(10)); // retriggered below
        list.push(7, cc(2));
        list.remove_note_on(10);
        list.push(7, cc(3));
        list.push(7, on(11));
        list.push(7, cc(4));
        let got: Vec<_> = list.finish().iter().map(|e| e.kind).collect();
        assert_eq!(got, [cc(1), cc(2), cc(3), cc(4), on(11)]);
    }

    /// Random pushes and removals: events at equal offsets come out in send order, note-ons last.
    #[test]
    fn event_list_is_fifo_at_equal_offsets_under_random_use() {
        let mut rng = Rng::new(0x226);
        for round in 0..200 {
            let mut list = EventList::new(64);
            let mut model: Vec<(u32, bool, u32, EventKind)> = Vec::new(); // offset, is_on, tag, kind
            for tag in 0..120u32 {
                if rng.below(4) == 0 {
                    if let Some(i) = model.iter().position(|m| m.1) {
                        let EventKind::NoteOn { note_id, .. } = model[i].3 else {
                            unreachable!()
                        };
                        list.remove_note_on(note_id);
                        model.remove(i);
                    }
                    continue;
                }
                let offset = rng.below(3) as u32;
                let is_on = rng.below(2) == 0;
                let kind = if is_on {
                    EventKind::NoteOn {
                        key: 60,
                        velocity: 1,
                        channel: 0,
                        note_id: tag,
                    }
                } else {
                    EventKind::Midi([0xb0, 1, (tag % 128) as u8])
                };
                if list.push(offset, kind) {
                    model.push((offset, is_on, tag, kind));
                }
            }
            model.sort_by_key(|m| (m.0, m.1)); // stable: keeps send order inside a group
            let want: Vec<_> = model.iter().map(|m| (m.0, m.3)).collect();
            let got: Vec<_> = list.finish().iter().map(|e| (e.offset, e.kind)).collect();
            assert_eq!(got, want, "round {round}");
        }
    }

    #[test]
    fn notes_start_and_end_on_their_samples_across_calls() {
        let notes = [note(10, 30, 60), note(25, 1, 62)];
        let mut run = Run::new();
        let mut all = Vec::new();
        for frames in [333, 1, 700, 2000] {
            all.extend(run.call(&notes, &[], frames, Some(SLOT)));
        }
        assert_eq!(
            keys(&all),
            [
                (250, "on", 60),
                (625, "on", 62),
                (650, "off", 62),
                (1000, "off", 60),
            ]
        );
        assert_eq!(run.seq.sounding(), 0);
        // Ids pair on and off.
        let id = |k: &EventKind| match *k {
            EventKind::NoteOn { note_id, .. } | EventKind::NoteOff { note_id, .. } => note_id,
            _ => unreachable!(),
        };
        assert_eq!(id(&all[0].1), id(&all[3].1));
        assert_eq!(id(&all[1].1), id(&all[2].1));
        assert_ne!(id(&all[0].1), id(&all[1].1));
    }

    #[test]
    fn retrigger_ends_the_sounding_note_first() {
        // Same key overlapping: the first ends where the second starts.
        let notes = [note(0, 100, 60), note(40, 100, 60)];
        let mut run = Run::new();
        let all = run.call(&notes, &[], 10_000, Some(SLOT));
        assert_eq!(
            keys(&all),
            [
                (0, "on", 60),
                (1000, "off", 60),
                (1000, "on", 60),
                (3500, "off", 60)
            ]
        );
        // A note that ended earlier in the same call ends at its own time.
        let notes = [note(0, 10, 60), note(40, 10, 60)];
        let mut run = Run::new();
        let all = run.call(&notes, &[], 10_000, Some(SLOT));
        assert_eq!(
            keys(&all),
            [
                (0, "on", 60),
                (250, "off", 60),
                (1000, "on", 60),
                (1250, "off", 60)
            ]
        );
    }

    #[test]
    fn zero_length_notes_still_end_after_they_start() {
        let notes = [note(10, 0, 60)];
        let mut run = Run::new();
        let all = run.call(&notes, &[], 1000, Some(SLOT));
        assert_eq!(keys(&all), [(250, "on", 60), (251, "off", 60)]);
    }

    #[test]
    fn flush_ends_notes_at_the_next_call() {
        let notes = [note(0, 1000, 60)];
        let live = [LiveMsg {
            offset: 3,
            data: [0x91, 64, 90],
            play: true,
            capture: false,
        }];
        let mut run = Run::new();
        run.call(&notes, &live, 100, Some(SLOT));
        assert_eq!(run.seq.sounding(), 2);
        // A loop wrap ends sequencer notes only.
        run.seq.flush(false);
        let got = run.call(&[], &[], 10, Some(SLOT));
        assert_eq!(keys(&got), [(100, "off", 60)]);
        // Stop ends live notes too.
        run.seq.flush(true);
        let got = run.call(&[], &[], 10, Some(SLOT));
        assert_eq!(keys(&got), [(110, "off", 64)]);
        assert_eq!(run.seq.sounding(), 0);
    }

    #[test]
    fn note_offs_only_reach_the_slot_that_has_the_note() {
        let notes = [note(0, 10, 60)];
        let mut run = Run::new();
        run.call(&notes, &[], 100, Some(SLOT));
        // The instrument changed: the old note ends silently.
        let got = run.call(&notes, &[], 1000, Some(SlotId(9)));
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(run.seq.sounding(), 0);
        // Without an instrument nothing plays, but the rolls go on.
        let mut run = Run::new();
        let got = run.call(&notes, &[], 1000, None);
        assert!(got.is_empty());
        assert_eq!(run.seq.sounding(), 0);
    }

    #[test]
    fn probability_rolls_and_follows() {
        let mut notes = Vec::new();
        for i in 0..1000 {
            let start = i * 40;
            notes.push(SeqNote {
                probability: 50,
                ..note(start, 5, 36)
            });
            // A ratchet hit following the roll, and a never and an always note.
            notes.push(SeqNote {
                probability: PROBABILITY_FOLLOW,
                ..note(start + 10, 5, 36)
            });
            notes.push(SeqNote {
                probability: 0,
                ..note(start + 20, 5, 37)
            });
            notes.push(SeqNote {
                probability: 100,
                ..note(start + 30, 5, 38)
            });
        }
        let play = |seed| {
            let mut run = Run::new();
            run.seq.reseed(seed);
            let mut events = Vec::new();
            for _ in 0..1000 {
                events.extend(run.call(&notes, &[], 40 * 25, Some(SLOT)));
            }
            keys(&events)
                .into_iter()
                .filter(|e| e.1 == "on")
                .map(|(s, _, k)| (s / 25, k))
                .collect::<Vec<_>>()
        };
        let a = play(7);
        assert_eq!(a, play(7), "same seed, same outcome");
        assert_ne!(a, play(8));
        let rolled: Vec<i64> = a
            .iter()
            .filter(|(t, k)| *k == 36 && t % 40 == 0)
            .map(|(t, _)| *t)
            .collect();
        let followed: Vec<i64> = a
            .iter()
            .filter(|(t, k)| *k == 36 && t % 40 == 10)
            .map(|(t, _)| t - 10)
            .collect();
        assert_eq!(rolled, followed);
        assert!((400..600).contains(&rolled.len()), "{}", rolled.len());
        assert!(a.iter().all(|(_, k)| *k != 37));
        assert_eq!(a.iter().filter(|(_, k)| *k == 38).count(), 1000);
    }

    #[test]
    fn live_notes_pair_and_capture() {
        let msg = |offset, data, play, capture| LiveMsg {
            offset,
            data,
            play,
            capture,
        };
        let live = [
            msg(1, [0x90, 60, 100], true, true),
            msg(2, [0x90, 62, 80], false, true),
            msg(3, [0xb0, 64, 127], true, false),
            msg(4, [0x90, 60, 0], true, true),
            msg(5, [0x80, 62, 0], false, true),
            // Released but never captured or played: nothing.
            msg(6, [0x80, 65, 0], true, true),
        ];
        let mut run = Run::new();
        let got = run.call(&[], &live, 10, Some(SLOT));
        assert_eq!(keys(&got), [(1, "on", 60), (3, "midi", 64), (4, "off", 60)]);
        assert_eq!(
            run.captured.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
            [
                Captured::On {
                    offset: 1,
                    key: 60,
                    velocity: 100
                },
                Captured::On {
                    offset: 2,
                    key: 62,
                    velocity: 80
                },
                Captured::Off { offset: 4, key: 60 },
                Captured::Off { offset: 5, key: 62 },
            ]
        );
        // Held captures close at stop.
        let live = [msg(0, [0x90, 70, 1], false, true)];
        run.call(&[], &live, 10, Some(SLOT));
        let mut closed = Vec::new();
        run.seq.close_captures(|k| closed.push(k));
        assert_eq!(closed, [70]);
    }

    #[test]
    fn notes_starting_and_ending_on_one_sample_never_invert() {
        let live = |offset, velocity| LiveMsg {
            offset,
            data: [0x90, 60, velocity],
            play: true,
            capture: false,
        };
        // Two note-ons for one key on one sample: only the second sounds.
        let mut run = Run::new();
        let got = run.call(&[], &[live(5, 1), live(5, 127)], 10, Some(SLOT));
        assert_eq!(keys(&got), [(5, "on", 60)]);
        assert!(matches!(got[0].1, EventKind::NoteOn { velocity: 127, .. }));
        // Pressed and released on one sample: the note-off waits for the next call.
        let mut run = Run::new();
        let got = run.call(&[], &[live(0, 100), live(0, 0)], 10, Some(SLOT));
        assert_eq!(keys(&got), [(0, "on", 60)]);
        let got = run.call(&[], &[], 10, Some(SLOT));
        assert_eq!(keys(&got), [(10, "off", 60)]);
        assert_eq!(run.seq.sounding(), 0);
        // Duplicate sequencer notes play once.
        let mut run = Run::new();
        let got = run.call(&[note(0, 10, 60), note(0, 10, 60)], &[], 1000, Some(SLOT));
        assert_eq!(keys(&got), [(0, "on", 60), (250, "off", 60)]);
    }

    #[test]
    fn live_and_sequenced_notes_merge_in_time_order() {
        // The live note at offset 100 retriggers the sequenced key held since offset 50.
        let notes = [note(2, 100, 60)];
        let live = [LiveMsg {
            offset: 100,
            data: [0x90, 60, 99],
            play: true,
            capture: false,
        }];
        let mut run = Run::new();
        let got = run.call(&notes, &live, 200, Some(SLOT));
        assert_eq!(
            keys(&got),
            [(50, "on", 60), (100, "off", 60), (100, "on", 60)]
        );
    }

    #[test]
    fn a_full_table_drops_note_ons() {
        let notes: Vec<SeqNote> = (0..300)
            .map(|i| note(i, 100_000, (i % 128) as u8))
            .collect();
        // Different channels so they do not retrigger each other.
        let notes: Vec<SeqNote> = notes
            .into_iter()
            .enumerate()
            .map(|(i, n)| SeqNote {
                channel: (i / 128) as u8,
                ..n
            })
            .collect();
        let mut run = Run::new();
        let win = Window {
            map: &run.map,
            rate: 48_000.0,
            start: 0,
            frames: 20_000,
            playing: true,
        };
        let dropped = run.seq.sequence(
            NoteView::from_slice(&notes),
            &[],
            &win,
            Some(SLOT),
            &mut run.ids,
            &mut run.repeat_scratch,
            &mut run.out,
            &mut |_| {},
        );
        assert_eq!(run.seq.sounding(), MAX_ACTIVE_NOTES);
        assert_eq!(dropped as usize, 300 - MAX_ACTIVE_NOTES);
    }

    #[test]
    fn seeds_differ_per_track_and_base() {
        let a = track_seed(1, TrackId(1));
        assert_ne!(a, track_seed(1, TrackId(2)));
        assert_ne!(a, track_seed(2, TrackId(1)));
        assert_eq!(a, track_seed(1, TrackId(1)));
    }

    #[test]
    fn note_ids_skip_none() {
        let mut ids = NoteIds(NOTE_ID_NONE - 1);
        assert_eq!(ids.next(), NOTE_ID_NONE - 1);
        assert_eq!(ids.next(), 0);
    }
}
