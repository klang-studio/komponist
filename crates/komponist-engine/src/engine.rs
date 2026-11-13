//! The realtime engine (ARCHITECTURE §6.1, §6.4).
//!
//! `Engine::process` runs on the audio thread, called by the app's `AudioProcess` implementation
//! (`komponist-audio` drives it; the engine does not know that crate). Per block of at most
//! [`crate::MAX_BLOCK`] frames (larger device buffers are cut into chunks):
//!
//! 1. Drain commands (bounded).
//! 2. Live MIDI input → monitored tracks (and capture while recording).
//! 3. Transport: the block's tick range, split at the loop end and at the end of the count-in;
//!    metronome clicks (accent on beat 1).
//! 4. Per track: sequencer notes from the program (note-offs from the active-notes table) + live
//!    events → processor chain → fader gain ramp → balance → meter → master bus (after the PDC
//!    delay).
//! 5. Master: inserts → master fader → meter → output; the metronome is added after the master
//!    fader with its own level.
//! 6. Telemetry and events.
//!
//! Realtime rules: no allocation, freeing, locking, logging, IO or blocking syscalls inside
//! `process` and `render` (only the futex wait of external processors, with a deadline). Everything
//! retired goes back as `Garbage`. FTZ/DAZ is set by the audio thread (`komponist-audio`).
//!
//! Export and tests run the engine without an audio device through [`Engine::render`]. To export
//! while a stream runs, the app takes the engine out of the stream with `komponist-audio`'s detach
//! and puts it back afterwards (this replaces the `Detach` command of ARCHITECTURE §6.3).
//!
//! Behaviour in detail:
//! - Segments: a block is processed in segments that end at the loop end, at the end of the
//!   count-in and after `max_block` frames (see the private `transport` module). Every processor is called once
//!   per segment, with the transport of the segment's first frame, so a loop wrap is sample-exact
//!   for events and transport alike.
//! - Stop and locate (while playing): note-offs for all sounding notes and `AllNotesOff` at offset
//!   0 of the next call, and `reset` on every processor of the program right away. A loop wrap
//!   ends the sequenced notes with note-offs at the wrap and does **not** reset (tails of
//!   reverbs and releases go on). Panic (`Command::AllNotesOff`) sends note-offs and
//!   `AllNotesOff`, without reset.
//! - Program swap: per-track state (active notes, probability RNG, fader position, fast-path
//!   values, delay line) follows the track id. Fast-path values end with the swap. A device whose
//!   bypass turns on gets `reset` once.
//! - Probability: each track rolls with its own `Rng`, seeded from [`EngineConfig::seed`] and the
//!   track id at every playback start.
//! - Live input: MIDI goes to monitored tracks whose input filter accepts it; `LiveNote` goes to
//!   its track regardless of monitoring. Armed tracks capture while recording. Captured keys
//!   still down get their `RecordedNoteOff` at the playhead on `Stop`, `StopRecording` and
//!   `Locate`, and at the loop end when the loop wraps (each lap is a take of its own).
//! - Instruments get note, MIDI, parameter and `AllNotesOff` events; effects get their parameters
//!   and `AllNotesOff`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use klang_dsp::buffer::{add, peak};
use komponist_model::{DeviceRole, Tick, TrackId};

use crate::api::{Command, EngineEvent, EngineHandle, Garbage, Telemetry, TransportState};
use crate::garbage::Outbox;
use crate::metronome::Metronome;
use crate::mixer::{DelayLine, Fader, audible, targets};
use crate::processor::{EventKind, ProcessContext, ProcessorError};
use crate::program::{ChainSlot, InputFilter, MidiPort, Program, SlotId};
use crate::sequencer::{Captured, EventList, LiveMsg, NoteIds, TrackSeq, Window, track_seed};
use crate::slots::{Buf, Bufs, Slot, run_chain};
use crate::transport::{Song, Transport};
use crate::{
    COMMAND_CAPACITY, EVENT_CAPACITY, MAX_BLOCK, MAX_COMMANDS_PER_BLOCK, MAX_EVENTS_PER_BLOCK,
    MAX_SLOTS, MAX_TRACKS,
};

/// Engine settings fixed at creation (the sample rate can change later with
/// [`Engine::set_sample_rate`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineConfig {
    /// Sample rate in Hz.
    pub sample_rate: f32,
    /// Largest number of frames per processor call, 1 ..= [`crate::MAX_BLOCK`]; processors are
    /// activated with it.
    pub max_block: usize,
    /// Base seed of the per-track probability RNGs (reseeded from it at every playback start, so
    /// offline renders are reproducible).
    pub seed: u32,
}

impl Default for EngineConfig {
    /// 48 kHz, `MAX_BLOCK`, seed 1.
    fn default() -> EngineConfig {
        EngineConfig {
            sample_rate: 48_000.0,
            max_block: crate::MAX_BLOCK,
            seed: 1,
        }
    }
}

/// A live MIDI message for this callback, already mapped to a frame offset by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiInEvent {
    /// Frame offset within the `process` call, `0 .. frames`.
    pub frame: u32,
    /// Port it came from.
    pub port: MidiPort,
    /// MIDI 1.0 channel message (status, data1, data2; unused bytes 0). Note-on with velocity 0 is
    /// a note-off. System messages are not passed in.
    pub data: [u8; 3],
}

/// Live input of one `process` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveInput<'a> {
    /// Live MIDI messages sorted by `frame`.
    pub midi: &'a [MidiInEvent],
    /// Output latency of the stream in frames (from the callback timing). Recorded ticks are
    /// corrected by it, so a note played in time with what the player hears lands on the beat.
    pub output_latency: u32,
}

/// The engine. Lives in a `Box` owned by the audio callback (or a render worker).
pub struct Engine {
    core: Core,
    render_l: Box<[f32]>,
    render_r: Box<[f32]>,
    render_out: Box<[f32]>,
}

impl Engine {
    /// Non-RT. Creates the engine with empty program and slot table (all capacities preallocated:
    /// [`crate::MAX_TRACKS`], [`crate::MAX_SLOTS`], …) and the UI's handle to it.
    ///
    /// Panics if the sample rate is not a positive finite number. `max_block` is clamped to
    /// `1 ..= MAX_BLOCK`.
    pub fn new(config: EngineConfig) -> (Box<Engine>, EngineHandle) {
        assert!(
            config.sample_rate.is_finite() && config.sample_rate > 0.0,
            "invalid sample rate {}",
            config.sample_rate
        );
        let (command_tx, command_rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
        let (event_tx, event_rx) = rtrb::RingBuffer::new(EVENT_CAPACITY);
        let telemetry = Arc::new(Telemetry::new());
        let core = Core {
            sample_rate: config.sample_rate,
            max_block: config.max_block.clamp(1, MAX_BLOCK),
            seed: config.seed,
            commands: command_rx,
            outbox: Outbox::new(event_tx),
            telemetry: telemetry.clone(),
            program: None,
            song: Song::default(),
            transport: Transport::default(),
            tracks: (0..MAX_TRACKS).map(|_| TrackState::new()).collect(),
            map: vec![0; MAX_TRACKS].into_boxed_slice(),
            slots: (0..MAX_SLOTS).map(|_| Slot::empty()).collect(),
            master_fader: Fader::at((1.0, 1.0)),
            master_gain: None,
            metronome: Metronome::default(),
            bufs: Bufs::new(),
            events: EventList::new(MAX_EVENTS_PER_BLOCK),
            fx_events: EventList::new(MAX_EVENTS_PER_BLOCK),
            live: Vec::with_capacity(MAX_EVENTS_PER_BLOCK),
            live_notes: Vec::with_capacity(MAX_COMMANDS_PER_BLOCK),
            note_ids: NoteIds::default(),
            unreported: false,
            dropped: 0,
        };
        let engine = Box::new(Engine {
            core,
            render_l: vec![0.0; MAX_BLOCK].into_boxed_slice(),
            render_r: vec![0.0; MAX_BLOCK].into_boxed_slice(),
            render_out: vec![0.0; 2 * MAX_BLOCK].into_boxed_slice(),
        });
        let handle = EngineHandle {
            commands: command_tx,
            events: event_rx,
            telemetry,
        };
        (engine, handle)
    }

    /// The current sample rate in Hz.
    pub fn sample_rate(&self) -> f32 {
        self.core.sample_rate
    }

    /// Non-RT (engine out of the audio callback). Changes the sample rate and re-activates every
    /// processor. Processors that fail stay in their slot but are treated as failed; their slots
    /// are returned.
    ///
    /// The playhead keeps its musical position. Panics if the rate is not a positive finite
    /// number.
    pub fn set_sample_rate(
        &mut self,
        sample_rate: f32,
    ) -> Result<(), Vec<(SlotId, ProcessorError)>> {
        assert!(
            sample_rate.is_finite() && sample_rate > 0.0,
            "invalid sample rate {sample_rate}"
        );
        let core = &mut self.core;
        let map = core.song.map;
        let (from, to) = (f64::from(core.sample_rate), f64::from(sample_rate));
        core.transport.rescale((&map, from), (&map, to));
        core.sample_rate = sample_rate;
        core.metronome.silence();
        let mut errors = Vec::new();
        for (index, slot) in core.slots.iter_mut().enumerate() {
            if let Some(processor) = slot.processor.as_mut()
                && let Err(error) = processor.activate(sample_rate, core.max_block)
            {
                // Reported through the return value, not as `ProcessorFailed`.
                slot.failed = true;
                slot.reported = true;
                errors.push((SlotId(index as u32), error));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// RT. Renders `out_l.len()` frames (any length; `out_l` and `out_r` have equal length) of the
    /// master output into the two planar buffers, overwriting them. `input` carries this callback's
    /// live MIDI.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32], input: &LiveInput<'_>) {
        self.core
            .run(out_l, out_r, input.midi, input.output_latency, false);
    }

    /// RT-safe, for offline use. Processes `frames` (1 ..= [`crate::MAX_BLOCK`]) frames with
    /// `offline = true` and no live input, and returns them interleaved stereo (`[l0, r0, l1, …]`,
    /// `2 × frames` samples) from an internal buffer valid until the next call. Panics if `frames`
    /// is 0 or larger than `MAX_BLOCK`.
    pub fn render(&mut self, frames: usize) -> &[f32] {
        assert!(
            (1..=MAX_BLOCK).contains(&frames),
            "render: {frames} frames, expected 1 ..= {MAX_BLOCK}"
        );
        let (l, r) = (&mut self.render_l[..frames], &mut self.render_r[..frames]);
        self.core.run(l, r, &[], 0, true);
        let out = &mut self.render_out[..2 * frames];
        let (frames_out, _) = out.as_chunks_mut::<2>();
        for (frame, (l, r)) in frames_out.iter_mut().zip(l.iter().zip(r.iter())) {
            frame[0] = *l;
            frame[1] = *r;
        }
        out
    }
}

impl core::fmt::Debug for Engine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

/// Runtime state of one track, kept across program swaps by track id.
struct TrackState {
    /// The track this state belongs to (`None`: free).
    id: Option<TrackId>,
    seq: TrackSeq,
    fader: Fader,
    /// Fast-path values (`SetTrackGain` …) until the next program.
    gain: Option<f32>,
    pan: Option<f32>,
    mute: Option<bool>,
    solo: Option<bool>,
    pdc: DelayLine,
}

impl TrackState {
    fn new() -> TrackState {
        TrackState {
            id: None,
            seq: TrackSeq::new(),
            fader: Fader::at((0.0, 0.0)),
            gain: None,
            pan: None,
            mute: None,
            solo: None,
            pdc: DelayLine::new(),
        }
    }

    fn clear_fast_path(&mut self) {
        self.gain = None;
        self.pan = None;
        self.mute = None;
        self.solo = None;
    }
}

/// Everything `process` touches.
struct Core {
    sample_rate: f32,
    max_block: usize,
    seed: u32,
    commands: rtrb::Consumer<Command>,
    outbox: Outbox,
    telemetry: Arc<Telemetry>,
    program: Option<Box<Program>>,
    song: Song,
    transport: Transport,
    /// Track states; `map[i]` is the state of program track `i`.
    tracks: Box<[TrackState]>,
    map: Box<[u16]>,
    slots: Box<[Slot]>,
    master_fader: Fader,
    master_gain: Option<f32>,
    metronome: Metronome,
    bufs: Bufs,
    /// Events of the current instrument call.
    events: EventList,
    /// Events of the current effect call.
    fx_events: EventList,
    /// Live messages of the current track and segment.
    live: Vec<LiveMsg>,
    /// `LiveNote` commands for the next block: track, key, velocity.
    live_notes: Vec<(TrackId, u8, u8)>,
    note_ids: NoteIds,
    /// A processor failed and `ProcessorFailed` has not gone out yet.
    unreported: bool,
    /// Events dropped in this block (per-call event lists, full queues inside the engine).
    dropped: u32,
}

/// Whether a command retires an object (and therefore needs room in the garbage holding list).
fn retires(command: &Command) -> bool {
    matches!(
        command,
        Command::SetProgram(_) | Command::InsertProcessor { .. } | Command::RemoveProcessor { .. }
    )
}

/// Whether a track listening with `filter` receives `m`.
fn accepts(filter: &InputFilter, m: &MidiInEvent) -> bool {
    let status = m.data[0];
    !filter.port_missing
        && (0x80..0xf0).contains(&status)
        && filter.port.is_none_or(|p| p == m.port)
        && filter.channel.is_none_or(|c| c == status & 0x0f)
}

impl Core {
    /// Number of program tracks the engine plays.
    fn track_count(&self) -> usize {
        self.program
            .as_ref()
            .map_or(0, |p| p.tracks.len().min(MAX_TRACKS))
    }

    /// RT. One `process` or `render` call.
    fn run(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        midi: &[MidiInEvent],
        latency: u32,
        offline: bool,
    ) {
        self.outbox.flush();
        self.drain_commands();
        if self.unreported {
            self.report_failures();
        }
        let n = out_l.len().min(out_r.len());
        let rate = f64::from(self.sample_rate);
        let mut done = 0;
        let mut midi_next = 0;
        while done < n {
            if let Some(at) = self.transport.count_in_done(&self.song, rate) {
                self.send(EngineEvent::CountInDone { at });
            }
            let max = (n - done).min(self.max_block);
            let (len, end) = self.transport.next_segment(max, &self.song, rate);
            let seg_end = done + len;
            let midi_first = midi_next;
            midi_next = if seg_end == n {
                midi.len()
            } else {
                midi_first + midi[midi_first..].partition_point(|m| (m.frame as usize) < seg_end)
            };
            self.segment(
                &mut out_l[done..seg_end],
                &mut out_r[done..seg_end],
                &midi[midi_first..midi_next],
                done,
                latency,
                offline,
            );
            if let Some(to) = self.transport.advance(len, end, &self.song, rate) {
                if self.transport.state == TransportState::Recording {
                    // Every lap is its own take: keys held across the wrap end at the loop end.
                    self.close_captures(self.song.loop_range.end);
                }
                let count = self.track_count();
                for &state in &self.map[..count] {
                    self.tracks[usize::from(state)].seq.flush(false);
                }
                self.send(EngineEvent::LoopWrapped { to });
            }
            done = seg_end;
        }
        out_l[n..].fill(0.0);
        out_r[n..].fill(0.0);
        self.live_notes.clear();
        if self.unreported {
            self.report_failures();
        }
        let t = &self.telemetry;
        let playhead = self.transport.tick(&self.song, rate);
        t.playhead.store(playhead.0, Ordering::Relaxed);
        t.state.store(self.transport.state as u8, Ordering::Relaxed);
        if self.dropped > 0 {
            t.dropped_events.fetch_add(self.dropped, Ordering::Relaxed);
            self.dropped = 0;
        }
    }

    /// RT. Applies queued commands (at most `MAX_COMMANDS_PER_BLOCK`, in order). Stops early at a
    /// command that retires something while the garbage holding list is full.
    fn drain_commands(&mut self) {
        for _ in 0..MAX_COMMANDS_PER_BLOCK {
            match self.commands.peek() {
                Ok(next) if retires(next) && !self.outbox.can_retire() => break,
                Ok(_) => {}
                Err(_) => break,
            }
            let Ok(command) = self.commands.pop() else {
                break;
            };
            self.apply(command);
        }
    }

    fn send(&mut self, event: EngineEvent) {
        self.outbox.send(event, &self.telemetry);
    }

    /// RT. Applies one command.
    fn apply(&mut self, command: Command) {
        let rate = f64::from(self.sample_rate);
        match command {
            Command::Play => {
                if !self.transport.playing() {
                    self.start(TransportState::Playing);
                }
            }
            Command::Stop => {
                self.close_captures(self.transport.tick(&self.song, rate));
                self.transport.state = TransportState::Stopped;
                self.silence(true);
            }
            Command::Locate(tick) => {
                if self.transport.playing() {
                    self.close_captures(self.transport.tick(&self.song, rate));
                    self.silence(true);
                }
                let tick = tick.max(Tick::ZERO);
                self.transport.pos = self.song.map.tick_to_sample(tick, rate);
                self.transport.wrapped = false;
            }
            Command::Record { from } => match self.transport.state {
                TransportState::Stopped => {
                    let from = from.max(Tick::ZERO);
                    let bars = i64::from(self.song.count_in_bars);
                    let start = Tick(from.0 - bars * self.song.map.meter.ticks_per_bar());
                    self.transport.pos = self.song.map.tick_to_sample(start, rate);
                    self.transport.record_from = from;
                    self.start(TransportState::CountIn);
                }
                TransportState::Playing => {
                    self.transport.state = TransportState::Recording;
                    let at = self.transport.tick(&self.song, rate);
                    self.send(EngineEvent::CountInDone { at });
                }
                TransportState::CountIn | TransportState::Recording => {}
            },
            Command::StopRecording => match self.transport.state {
                TransportState::Recording => {
                    self.close_captures(self.transport.tick(&self.song, rate));
                    self.transport.state = TransportState::Playing;
                }
                TransportState::CountIn => self.transport.state = TransportState::Playing,
                TransportState::Stopped | TransportState::Playing => {}
            },
            Command::SetProgram(program) => self.install(program),
            Command::InsertProcessor { slot, processor } => {
                self.forget_slot(slot);
                match self.slots.get_mut(slot.0 as usize) {
                    Some(entry) => {
                        let old = entry.processor.replace(processor);
                        entry.failed = false;
                        entry.reported = false;
                        entry.all_off = false;
                        entry.params.clear();
                        if let Some(old) = old {
                            self.outbox.retire(Garbage::Processor(slot, old));
                        }
                    }
                    None => self.outbox.retire(Garbage::Processor(slot, processor)),
                }
            }
            Command::RemoveProcessor { slot } => {
                self.forget_slot(slot);
                if let Some(entry) = self.slots.get_mut(slot.0 as usize)
                    && let Some(old) = entry.processor.take()
                {
                    entry.failed = false;
                    entry.reported = false;
                    entry.all_off = false;
                    entry.params.clear();
                    self.outbox.retire(Garbage::Processor(slot, old));
                }
            }
            Command::SetParam { slot, id, value } => {
                if let Some(entry) = self.slots.get_mut(slot.0 as usize)
                    && !entry.params.set(id, value)
                {
                    self.dropped += 1;
                }
            }
            Command::SetTrackGain { track, gain } => {
                if let Some(state) = self.state_of(track) {
                    state.gain = Some(gain);
                }
            }
            Command::SetTrackPan { track, pan } => {
                if let Some(state) = self.state_of(track) {
                    state.pan = Some(pan);
                }
            }
            Command::SetTrackMute { track, mute } => {
                if let Some(state) = self.state_of(track) {
                    state.mute = Some(mute);
                }
            }
            Command::SetTrackSolo { track, solo } => {
                if let Some(state) = self.state_of(track) {
                    state.solo = Some(solo);
                }
            }
            Command::SetMasterGain(gain) => self.master_gain = Some(gain),
            Command::LiveNote {
                track,
                key,
                velocity,
            } => {
                if key <= 127 && self.live_notes.len() < self.live_notes.capacity() {
                    self.live_notes.push((track, key, velocity.min(127)));
                } else {
                    self.dropped += 1;
                }
            }
            Command::AllNotesOff => self.silence(false),
        }
    }

    /// RT. Drops the active notes of every track held by `slot`.
    fn forget_slot(&mut self, slot: SlotId) {
        let count = self.track_count();
        for &index in &self.map[..count] {
            self.tracks[usize::from(index)].seq.forget_slot(slot);
        }
    }

    /// The state of the program track `track`.
    fn state_of(&mut self, track: TrackId) -> Option<&mut TrackState> {
        let program = self.program.as_deref()?;
        let index = program.tracks[..program.tracks.len().min(MAX_TRACKS)]
            .iter()
            .position(|t| t.id == track)?;
        Some(&mut self.tracks[usize::from(self.map[index])])
    }

    /// The chain slots of the program: all tracks, then the master.
    fn program_slots(program: &Program) -> impl Iterator<Item = &ChainSlot> {
        program.tracks[..program.tracks.len().min(MAX_TRACKS)]
            .iter()
            .flat_map(|t| t.chain.iter())
            .chain(program.master.chain.iter())
    }

    /// RT. Starts the transport in `state` (`Playing` or `CountIn`) and reseeds the probability
    /// RNGs.
    fn start(&mut self, state: TransportState) {
        self.transport.state = state;
        self.transport.wrapped = false;
        let count = self.track_count();
        for &index in &self.map[..count] {
            let state = &mut self.tracks[usize::from(index)];
            if let Some(id) = state.id {
                state.seq.reseed(track_seed(self.seed, id));
            }
        }
    }

    /// RT. Note-offs for every sounding note and `AllNotesOff` to every processor of the program
    /// at the next call; with `reset`, every processor is also reset at once.
    fn silence(&mut self, reset: bool) {
        let count = self.track_count();
        for &index in &self.map[..count] {
            self.tracks[usize::from(index)].seq.flush(true);
        }
        let Some(program) = self.program.as_deref() else {
            return;
        };
        for device in Core::program_slots(program) {
            if let Some(slot) = self.slots.get_mut(device.slot.0 as usize) {
                slot.all_off = true;
                if reset && let Some(processor) = slot.processor.as_mut() {
                    processor.reset();
                }
            }
        }
    }

    /// RT. Sends `RecordedNoteOff` at `tick` for every captured key still down.
    fn close_captures(&mut self, tick: Tick) {
        let Some(program) = self.program.as_deref() else {
            return;
        };
        let count = program.tracks.len().min(MAX_TRACKS);
        for (track, &index) in program.tracks[..count].iter().zip(&self.map[..count]) {
            let (outbox, telemetry) = (&mut self.outbox, &self.telemetry);
            self.tracks[usize::from(index)].seq.close_captures(|key| {
                let event = EngineEvent::RecordedNoteOff {
                    track: track.id,
                    tick,
                    key,
                };
                outbox.send(event, telemetry);
            });
        }
    }

    /// RT. Installs a new program and retires the old one.
    fn install(&mut self, program: Box<Program>) {
        let rate = f64::from(self.sample_rate);
        let song = Song::of(&program);
        if song.map != self.song.map {
            self.transport
                .rescale((&self.song.map, rate), (&song.map, rate));
        }
        // Track states follow the track ids; new tracks get free states.
        let old_count = self.track_count();
        let count = program.tracks.len().min(MAX_TRACKS);
        let mut used = [false; MAX_TRACKS];
        let mut map = [u16::MAX; MAX_TRACKS];
        for (i, track) in program.tracks[..count].iter().enumerate() {
            for &index in &self.map[..old_count] {
                let index = usize::from(index);
                if !used[index] && self.tracks[index].id == Some(track.id) {
                    used[index] = true;
                    map[i] = index as u16;
                    self.tracks[index].clear_fast_path();
                    break;
                }
            }
        }
        let any_solo = program.tracks[..count].iter().any(|t| t.solo);
        let mut free = 0;
        for (i, track) in program.tracks[..count].iter().enumerate() {
            if map[i] != u16::MAX {
                continue;
            }
            while used[free] {
                free += 1;
            }
            used[free] = true;
            map[i] = free as u16;
            let state = &mut self.tracks[free];
            state.id = Some(track.id);
            state.seq.clear();
            state.seq.reseed(track_seed(self.seed, track.id));
            state.clear_fast_path();
            let audible = audible(track.mute, track.solo, any_solo);
            state.fader = Fader::at(targets(track.gain, track.pan, audible));
        }
        for (index, state) in self.tracks.iter_mut().enumerate() {
            if !used[index] {
                state.id = None;
            }
        }
        self.map[..count].copy_from_slice(&map[..count]);
        // A device whose bypass turns on is reset once.
        for device in Core::program_slots(&program) {
            if let Some(slot) = self.slots.get_mut(device.slot.0 as usize) {
                if device.bypass
                    && !slot.bypassed
                    && let Some(processor) = slot.processor.as_mut()
                {
                    processor.reset();
                }
                slot.bypassed = device.bypass;
            }
        }
        let master = &program.master;
        if self.program.is_none() {
            self.master_fader = Fader::at(targets(master.gain, master.pan, !master.mute));
        }
        self.master_gain = None;
        self.song = song;
        if let Some(old) = self.program.replace(program) {
            self.outbox.retire(Garbage::Program(old));
        }
    }

    /// RT. Sends `ProcessorFailed` for failed slots not reported yet (retried while the queue is
    /// full).
    fn report_failures(&mut self) {
        self.unreported = false;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.failed && !slot.reported {
                let event = EngineEvent::ProcessorFailed {
                    slot: SlotId(index as u32),
                };
                if self.outbox.send(event, &self.telemetry) {
                    slot.reported = true;
                } else {
                    self.unreported = true;
                    return;
                }
            }
        }
    }

    /// RT. Renders one segment into `out_l`/`out_r` (which it overwrites). `midi` holds the live
    /// messages of the segment, with frames counted from the start of the call; the segment
    /// starts at frame `frame0` of the call.
    fn segment(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        midi: &[MidiInEvent],
        frame0: usize,
        latency: u32,
        offline: bool,
    ) {
        let n = out_l.len();
        let sample_rate = self.sample_rate;
        let rate = f64::from(sample_rate);
        let song = self.song;
        let transport = self.transport;
        let ctx = ProcessContext {
            frames: n as u32,
            sample_rate,
            transport: transport.info(&song, rate),
            offline,
        };
        let win = Window {
            map: &song.map,
            rate,
            start: transport.pos,
            frames: n as u32,
            playing: transport.playing(),
        };
        let recording = transport.state == TransportState::Recording;
        let loop_samples = song.loop_samples(rate);
        // Song tick of captured input at `sample`, corrected by the output latency (and back over
        // the loop start after a wrap).
        let record_tick = |sample: i64| {
            let mut corrected = sample - i64::from(latency);
            if transport.wrapped
                && let Some((start, end)) = loop_samples
                && corrected < start
                && sample >= start
            {
                corrected += end - start;
            }
            song.map.sample_to_tick(corrected, rate)
        };

        let Core {
            program,
            tracks,
            map,
            slots,
            bufs,
            events,
            fx_events,
            live,
            live_notes,
            note_ids,
            outbox,
            telemetry,
            master_fader,
            master_gain,
            metronome,
            unreported,
            dropped,
            ..
        } = self;

        match program.as_deref() {
            None => {
                out_l.fill(0.0);
                out_r.fill(0.0);
            }
            Some(program) => {
                bufs.bus[0][..n].fill(0.0);
                bufs.bus[1][..n].fill(0.0);
                let count = program.tracks.len().min(MAX_TRACKS);
                let any_solo = program.tracks[..count]
                    .iter()
                    .zip(&map[..count])
                    .any(|(t, &s)| tracks[usize::from(s)].solo.unwrap_or(t.solo));
                for (track, &state) in program.tracks[..count].iter().zip(&map[..count]) {
                    let state = &mut tracks[usize::from(state)];
                    // Live input of this track.
                    live.clear();
                    let capture = track.arm && recording;
                    if frame0 == 0 {
                        for &(id, key, velocity) in live_notes.iter() {
                            if id == track.id && live.len() < live.capacity() {
                                let status = if velocity > 0 { 0x90 } else { 0x80 };
                                live.push(LiveMsg {
                                    offset: 0,
                                    data: [status, key, velocity],
                                    play: true,
                                    capture,
                                });
                            }
                        }
                    }
                    if track.monitor || capture {
                        for m in midi.iter().filter(|m| accepts(&track.input, m)) {
                            if live.len() == live.capacity() {
                                *dropped += 1;
                                break;
                            }
                            let frame = (m.frame as usize).clamp(frame0, frame0 + n - 1);
                            live.push(LiveMsg {
                                offset: (frame - frame0) as u32,
                                data: m.data,
                                play: track.monitor,
                                capture,
                            });
                        }
                    }
                    // The instrument's events.
                    let first = track
                        .chain
                        .first()
                        .filter(|d| d.role == DeviceRole::Instrument);
                    let instrument = first
                        .filter(|d| {
                            slots
                                .get(d.slot.0 as usize)
                                .is_some_and(|s| s.runs(d.bypass))
                        })
                        .map(|d| d.slot);
                    events.clear();
                    state.seq.flushed_offs(instrument, events);
                    if let Some(device) = first
                        && let Some(slot) = slots.get_mut(device.slot.0 as usize)
                    {
                        let all_off = core::mem::take(&mut slot.all_off);
                        if instrument.is_some() {
                            if all_off {
                                events.push(0, EventKind::AllNotesOff);
                            }
                            slot.params.drain_into(events, state.seq.reserved() + 2);
                        }
                    }
                    let id = track.id;
                    *dropped += state.seq.sequence(
                        &track.notes,
                        live,
                        &win,
                        instrument,
                        note_ids,
                        events,
                        &mut |captured| {
                            let event = match captured {
                                Captured::On {
                                    offset,
                                    key,
                                    velocity,
                                } => EngineEvent::RecordedNoteOn {
                                    track: id,
                                    tick: record_tick(win.start + i64::from(offset)),
                                    key,
                                    velocity,
                                },
                                Captured::Off { offset, key } => EngineEvent::RecordedNoteOff {
                                    track: id,
                                    tick: record_tick(win.start + i64::from(offset)),
                                    key,
                                },
                            };
                            outbox.send(event, telemetry);
                        },
                    );
                    // Chain, fader, balance, meter, delay compensation, bus.
                    bufs.a[0][..n].fill(0.0);
                    bufs.a[1][..n].fill(0.0);
                    let (result, failed) = run_chain(
                        &track.chain,
                        slots,
                        &ctx,
                        events.finish(),
                        fx_events,
                        bufs,
                        n,
                        true,
                    );
                    *unreported |= failed;
                    let gain = state.gain.unwrap_or(track.gain);
                    let pan = state.pan.unwrap_or(track.pan);
                    let mute = state.mute.unwrap_or(track.mute);
                    let solo = state.solo.unwrap_or(track.solo);
                    let target = targets(gain, pan, audible(mute, solo, any_solo));
                    let Bufs { a, b, bus, .. } = &mut *bufs;
                    let [l, r] = match result {
                        Buf::A => a,
                        Buf::B => b,
                    };
                    let (l, r) = (&mut l[..n], &mut r[..n]);
                    state.fader.apply(l, r, target);
                    track.meter.record(peak(l), peak(r));
                    state.pdc.process(l, r, track.pdc_delay);
                    add(&mut bus[0][..n], l);
                    add(&mut bus[1][..n], r);
                }
                // Master inserts, fader, meter, output.
                let Bufs { a, bus, .. } = &mut *bufs;
                a[0][..n].copy_from_slice(&bus[0][..n]);
                a[1][..n].copy_from_slice(&bus[1][..n]);
                let master = &program.master;
                let (result, failed) =
                    run_chain(&master.chain, slots, &ctx, &[], fx_events, bufs, n, false);
                *unreported |= failed;
                let (l, r) = bufs.pair(result, n);
                let gain = master_gain.unwrap_or(master.gain);
                master_fader.apply(l, r, targets(gain, master.pan, !master.mute));
                telemetry.master.record(peak(l), peak(r));
                out_l.copy_from_slice(l);
                out_r.copy_from_slice(r);
            }
        }
        // The metronome, after the master fader.
        if transport.state == TransportState::CountIn || (transport.playing() && song.metronome) {
            metronome.run(out_l, out_r, transport.pos, &song.map, sample_rate);
        } else {
            metronome.render(out_l, out_r);
        }
    }
}

// The engine moves into the audio callback.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Engine>();
};
