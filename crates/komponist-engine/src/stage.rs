//! The MIDI effect stage of a track (WP-88, ARCHITECTURE §6.4.2).
//!
//! A track chain is `[MIDI effects…] → instrument → [audio effects…]`. The sequencer and the live
//! input produce the track's events as before (phase 1, audio thread); the track's job then runs
//! them through the active stages ([`EventProcessor`]) and a **guard** before the instrument sees
//! them. The guard is the track's table of notes the instrument received through the stage:
//!
//! - every note-on that passes gets an engine-assigned id (a per-track counter, so the result does
//!   not depend on which thread runs the job);
//! - a note-off passes only for a note in the table, with that note's id; others are dropped;
//! - a note-on for a key that already sounds first ends the old note (retrigger);
//! - on stop, locate and panic (the instrument slot's `all_off`), on bypass, removal, replacement
//!   and reordering of a stage ([`StageState::flush_all`]) every note of the table ends with a
//!   note-off at offset 0 of the next call. A stage cannot hang a note, whatever it does.
//!
//! The sequencer's own table ([`crate::sequencer`]) keeps meaning "notes that entered the stage":
//! on a track with MIDI effects its notes are tagged with the slot of the first MIDI effect, so
//! its note-offs reach the stage chain (not the instrument) and survive program swaps as before.

use komponist_model::DeviceRole;
use komponist_model::time::PPQ;

use crate::MAX_STAGES;
use crate::automation;
use crate::processor::{Event, EventKind, EventProcessor, EventSink, ProcessContext};
use crate::program::{ChainSlot, SlotId};
use crate::sequencer::{EventList, MAX_ACTIVE_NOTES};
use crate::slots::SlotTable;

/// What decides how a track's events reach its instrument: the MIDI effect slots with their
/// bypass flags, and the instrument slot. A program swap that changes it cuts the track's notes
/// (see [`crate::engine`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StageSig {
    pub(crate) stages: u8,
    slots: [u32; MAX_STAGES],
    bypass: u8,
    pub(crate) instrument: Option<SlotId>,
}

impl StageSig {
    /// The signature of a track chain.
    pub(crate) fn of(chain: &[ChainSlot]) -> StageSig {
        let stages = chain
            .iter()
            .take_while(|d| d.role == DeviceRole::MidiEffect)
            .count()
            .min(MAX_STAGES);
        let mut sig = StageSig {
            stages: stages as u8,
            slots: [u32::MAX; MAX_STAGES],
            bypass: 0,
            instrument: chain
                .get(stages)
                .filter(|d| d.role == DeviceRole::Instrument)
                .map(|d| d.slot),
        };
        for (i, d) in chain[..stages].iter().enumerate() {
            sig.slots[i] = d.slot.0;
            sig.bypass |= u8::from(d.bypass) << i;
        }
        sig
    }

    /// Slot of the first MIDI effect: the tag of the sequencer's notes on a staged track.
    pub(crate) fn head(&self) -> Option<SlotId> {
        (self.stages > 0).then(|| SlotId(self.slots[0]))
    }
}

/// A note the instrument received through the stage.
#[derive(Debug, Clone, Copy)]
struct GuardNote {
    key: u8,
    channel: u8,
    note_id: u32,
    /// Note-off due at offset 0 of the next call.
    flushed: bool,
}

/// Per-track state of the stage: the guard's table and the signature of the installed chain.
pub(crate) struct StageState {
    notes: Vec<GuardNote>,
    next_id: u32,
    pub(crate) sig: StageSig,
}

impl StageState {
    /// Non-RT. Empty, with the table preallocated.
    pub(crate) fn new() -> StageState {
        StageState {
            notes: Vec::with_capacity(MAX_ACTIVE_NOTES),
            next_id: 0,
            sig: StageSig::of(&[]),
        }
    }

    /// RT. Forgets everything (the state goes to another track).
    pub(crate) fn clear(&mut self) {
        self.notes.clear();
    }

    /// Whether the guard still holds notes: the track's job must run the stage path to end them.
    pub(crate) fn holds_notes(&self) -> bool {
        !self.notes.is_empty()
    }

    /// RT. Every note of the table ends at offset 0 of the next call.
    pub(crate) fn flush_all(&mut self) {
        for n in &mut self.notes {
            n.flushed = true;
        }
    }

    /// RT. Takes over a note the instrument already holds (from the sequencer's table, when a
    /// stage is put in front of it); its note-off comes at offset 0 of the next call.
    pub(crate) fn adopt(&mut self, key: u8, channel: u8, note_id: u32) {
        if self.notes.len() < MAX_ACTIVE_NOTES {
            self.notes.push(GuardNote {
                key,
                channel,
                note_id,
                flushed: true,
            });
        }
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == crate::NOTE_ID_NONE {
            self.next_id = 0;
        }
        id
    }

    fn find(&self, channel: u8, key: u8) -> Option<usize> {
        self.notes
            .iter()
            .position(|n| n.channel == channel && n.key == key)
    }

    /// Note-offs at offset 0 for the flushed notes.
    fn emit_flushed(&mut self, out: &mut EventList) {
        let mut k = 0;
        while k < self.notes.len() {
            let n = self.notes[k];
            if n.flushed {
                out.push(0, off(&n));
                self.notes.swap_remove(k);
            } else {
                k += 1;
            }
        }
    }

    /// One event of the last stage's output, towards the instrument. Returns the number of
    /// dropped events (0 or 1).
    fn admit(&mut self, e: &Event, out: &mut EventList) -> u32 {
        match e.kind {
            EventKind::NoteOn {
                key,
                velocity,
                channel,
                ..
            } => {
                let (key, channel) = (key & 0x7f, channel & 0x0f);
                if let Some(k) = self.find(channel, key) {
                    let old = self.notes.swap_remove(k);
                    if out.note_on_offset(old.note_id) == Some(e.offset) {
                        // Both start on one sample: the old note never sounds.
                        out.remove_note_on(old.note_id);
                    } else {
                        out.push(e.offset, off(&old));
                    }
                }
                // Room for the note-offs of the table, this note's included.
                if self.notes.len() >= MAX_ACTIVE_NOTES || out.room() < self.notes.len() + 2 {
                    return 1;
                }
                let note_id = self.next_id();
                out.push(
                    e.offset,
                    EventKind::NoteOn {
                        key,
                        velocity: velocity.clamp(1, 127),
                        channel,
                        note_id,
                    },
                );
                self.notes.push(GuardNote {
                    key,
                    channel,
                    note_id,
                    flushed: false,
                });
                0
            }
            EventKind::NoteOff { key, channel, .. } => {
                let Some(k) = self.find(channel & 0x0f, key & 0x7f) else {
                    return 0;
                };
                let n = self.notes[k];
                if out.note_on_offset(n.note_id) == Some(e.offset) {
                    // Released on the sample it started: the note-off would sort before the
                    // note-on, so it goes out at the start of the next call.
                    self.notes[k].flushed = true;
                } else {
                    self.notes.swap_remove(k);
                    out.push(e.offset, off(&n));
                }
                0
            }
            EventKind::Midi(_) => {
                if out.room() <= self.notes.len() + 1 {
                    return 1;
                }
                out.push(e.offset, e.kind);
                0
            }
            EventKind::AllNotesOff => {
                for n in self.notes.drain(..) {
                    out.push(e.offset, off(&n));
                }
                out.push(e.offset, EventKind::AllNotesOff);
                0
            }
            // Parameters reach the instrument from the engine's queue, not from a stage.
            EventKind::Param { .. } | EventKind::ParamEdit { .. } => 0,
        }
    }
}

fn off(n: &GuardNote) -> EventKind {
    EventKind::NoteOff {
        key: n.key,
        channel: n.channel,
        note_id: n.note_id,
    }
}

/// Scratch event lists of one thread that runs track jobs (a lane).
pub(crate) struct StageLists {
    a: EventList,
    b: EventList,
    out: EventList,
    rebased: Vec<Event>,
}

impl StageLists {
    /// Non-RT.
    pub(crate) fn new(capacity: usize) -> StageLists {
        StageLists {
            a: EventList::new(capacity),
            b: EventList::new(capacity),
            out: EventList::new(capacity),
            rebased: Vec::with_capacity(capacity),
        }
    }
}

/// RT. The instrument's events of one call on a track with MIDI effects (or with notes left from
/// before): the active stages of `chain` run over `input` in order, the guard turns the result
/// into the instrument's events, which come with the engine's `AllNotesOff` and the instrument's
/// pending parameters. Returns them (sorted) and the number of dropped events.
///
/// `stages` is the number of leading MIDI effects of `chain`. With none, `input` passes through
/// and only the guard's leftover note-offs are added.
#[allow(clippy::too_many_arguments)] // Related to the track's prepared event lists.
pub(crate) fn run<'a, T: SlotTable + ?Sized>(
    chain: &[ChainSlot],
    stages: usize,
    slots: &mut T,
    ctx: &ProcessContext,
    input: &[Event],
    state: &mut StageState,
    lists: &'a mut StageLists,
    automation: &mut automation::State,
) -> (&'a [Event], u32) {
    let StageLists { a, b, out, rebased } = lists;
    out.clear();
    let inst = chain
        .get(stages)
        .filter(|d| d.role == DeviceRole::Instrument);
    let (mut inst_runs, mut all_off) = (false, false);
    if let Some(d) = inst
        && let Some(slot) = slots.slot_mut(d.slot.0 as usize)
    {
        inst_runs = slot.runs(d.bypass);
        all_off = core::mem::take(&mut slot.all_off);
    }
    if !inst_runs {
        // No instrument to play: nothing goes out, and nothing it held is left.
        state.clear();
        for d in &chain[..stages] {
            if let Some(slot) = slots.slot_mut(d.slot.0 as usize) {
                slot.all_off = false;
            }
        }
        return (out.finish(), 0);
    }
    if all_off {
        state.flush_all();
    }
    state.emit_flushed(out);
    if all_off {
        out.push(0, EventKind::AllNotesOff);
    }
    let mut dropped = 0;
    if stages == 0 {
        // Nothing to run: the sequencer's events go straight to the instrument (it has its own
        // note table there).
        for e in input {
            if !out.push(e.offset, e.kind) {
                dropped += 1;
            }
        }
    } else {
        // `last`: which list holds the result so far (`None`: still the input).
        let mut last: Option<bool> = None;
        for d in &chain[..stages] {
            let Some(slot) = slots.slot_mut(d.slot.0 as usize) else {
                continue;
            };
            let stage_all_off = core::mem::take(&mut slot.all_off);
            if !slot.runs(d.bypass) {
                continue;
            }
            let (params, processor) = (&mut slot.params, slot.processor.as_mut());
            let Some(stage) = processor.and_then(|p| p.event_processor()) else {
                continue;
            };
            if stage_all_off {
                stage.reset();
            }
            params.drain_with(|id, value, serial| match serial {
                Some(serial) => stage.set_param_edit(id, value, serial),
                None => stage.set_param(id, value),
            });
            let (src, dst): (&[Event], &mut EventList) = match last {
                None => (input, &mut *a),
                Some(false) => (a.sorted(), &mut *b),
                Some(true) => (b.sorted(), &mut *a),
            };
            dst.clear();
            process_stage(
                stage,
                ctx,
                src,
                dst,
                automation.stage_events(d.slot),
                rebased,
            );
            dst.finish();
            last = Some(matches!(last, Some(false)));
        }
        let result: &[Event] = match last {
            None => input,
            Some(false) => a.sorted(),
            Some(true) => b.sorted(),
        };
        for e in result {
            dropped += state.admit(e, out);
        }
    }
    if let Some(d) = inst
        && let Some(slot) = slots.slot_mut(d.slot.0 as usize)
    {
        slot.params.drain_into(out, state.notes.len() + 2);
        dropped += automation.add_events(d.slot, out);
    }
    (out.finish(), dropped)
}

/// Process a MIDI stage over contiguous windows ending at an automated parameter change. The
/// stage's immediate setter takes effect at the correct sample, before input at that offset.
fn process_stage(
    stage: &mut dyn EventProcessor,
    context: &ProcessContext,
    mut input: &[Event],
    output: &mut EventList,
    automation: impl Iterator<Item = Event>,
    rebased: &mut Vec<Event>,
) {
    let mut automation = automation.peekable();
    if automation.peek().is_none() {
        stage.process_events(context, input, &mut EventSink::new(output, context.frames));
        return;
    }
    let mut start = 0;
    while start < context.frames {
        while let Some(event) = automation.next_if(|event| event.offset == start) {
            if let EventKind::Param { id, value } = event.kind {
                stage.set_param(id, value);
            }
        }
        let end = automation
            .peek()
            .map_or(context.frames, |param| param.offset);
        let n = input.partition_point(|event| event.offset < end);
        let (events, left) = input.split_at(n);
        input = left;
        rebased.clear();
        rebased.extend(events.iter().map(|event| Event {
            offset: event.offset - start,
            ..*event
        }));
        let mut ctx = *context;
        ctx.frames = end - start;
        if ctx.transport.playing {
            ctx.transport.position_ticks += f64::from(start) * ctx.transport.tempo_bpm * PPQ as f64
                / (60.0 * f64::from(ctx.sample_rate));
        }
        stage.process_events(
            &ctx,
            rebased,
            &mut EventSink::in_segment(output, start, ctx.frames),
        );
        start = end;
    }
}
