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
//!    events → MIDI effect stage (`stage`, WP-88) → processor chain → fader gain ramp → balance →
//!    meter → master bus (after the PDC delay). With routing (WP-102, ARCHITECTURE §6.8.4) the
//!    tracks and buses are jobs of a graph in topological order: a bus first sums its inputs
//!    (outputs and sends of its sources, delay-compensated, in a fixed order), devices with a
//!    sidechain get their source's tap, and the master sums what outputs to it.
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
//! - Stop and locate (while playing), loop wrap, panic (`Command::AllNotesOff`): note-offs for
//!   all sounding notes and `AllNotesOff` at offset 0 of the next call (a loop wrap: note-offs at
//!   the wrap), and **no** `reset`: the engine keeps calling every processor while stopped, so
//!   releases, delays and reverbs ring out. MIDI effects get their `reset` through the
//!   `AllNotesOff` (they have no tail).
//! - Second Stop (`Command::Stop` while already stopped): the hard cut. Note-offs and
//!   `AllNotesOff` at once, and the final output (after master and metronome) fades linearly to
//!   0 over 5 ms (`CUT_SECONDS`), across block boundaries, so the cut does not click. When the
//!   fade is over the rest of that call is silent and every processor of the program is `reset`
//!   once, after the call. A `Play` or `Record` (from stop), a `SetProgram`, `InsertProcessor` or
//!   `RemoveProcessor` arriving during the fade finish the cut at once (reset, full level again);
//!   another `Stop` and a `Locate` leave it running. The app sends it when Stop is pressed while
//!   stopped; export sends two Stops and a `SetProgram` before it starts, which complete the cut
//!   in the same command drain, so the song's rendering begins without the live tails and
//!   without any ramp.
//! - Program swap: per-track state (active notes, probability RNG, fader position, fast-path
//!   values, delay line) follows the track id. Fast-path values end with the swap. A device whose
//!   bypass turns on gets `reset` once.
//! - Note chasing: after a playback start (`Play`, `Record` from stop), a `Locate` while playing
//!   and a loop wrap, notes that started earlier and still sound at the new position start at
//!   once with their remaining length (see the private `sequencer` module). Export plays through
//!   the same path, so a range starting inside a pad renders the pad.
//! - Probability: each track rolls with its own `Rng`, seeded from [`EngineConfig::seed`] and the
//!   track id at every playback start.
//! - Live input: MIDI goes to monitored tracks whose input filter accepts it; `LiveNote` goes to
//!   its track regardless of monitoring. A track monitors when I is on **or it is armed** (R,
//!   auto-monitoring, owner decision 2026-11-15); an armed track also captures while
//!   recording. Captured keys
//!   still down get their `RecordedNoteOff` at the playhead on `Stop`, `StopRecording` and
//!   `Locate`, and at the loop end when the loop wraps (each lap is a take of its own).
//! - Instruments get note, MIDI, parameter and `AllNotesOff` events; effects get their parameters
//!   and `AllNotesOff`. On a track with MIDI effects the instrument gets what the stage chain makes
//!   of the track's events, with note ids and note-offs guaranteed by the stage's guard
//!   (ARCHITECTURE §6.4.2); recording still captures the input.

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use klang_dsp::buffer::{add, peak};
use komponist_model::{Tap, Tick, TrackId};

use crate::api::{Command, EngineEvent, EngineHandle, Garbage, Telemetry, TransportState};
use crate::garbage::Outbox;
use crate::graph::EdgeState;
use crate::metronome::Metronome;
use crate::mixer::{DelayLine, Fader, audible, targets};
use crate::processor::{DEADLINE_SHARE, Event, EventKind, ProcessContext, ProcessorError};
use crate::program::{ChainSlot, InputFilter, MidiPort, Program, SlotId, TrackProgram};
use crate::route::{Delay, GateTable, MAX_LINES, NodeRoute, Routing, SidechainRoute};
use crate::sequencer::{Captured, EventList, LiveMsg, NoteIds, TrackSeq, Window, track_seed};
use crate::slots::{Bufs, ChainHook, SharedSlots, Slot, chains_exclusive, run_chain};
use crate::stage::{self, StageLists, StageSig, StageState};
use crate::transport::{Song, Transport};
use crate::workers::{MAX_WORKERS, Pool, Publisher, WorkerStats, WorkerStatus, run_serial};
use crate::{
    COMMAND_CAPACITY, EVENT_CAPACITY, MAX_BLOCK, MAX_COMMANDS_PER_BLOCK, MAX_EVENTS_PER_BLOCK,
    MAX_SLOTS, MAX_TRACKS,
};

/// Shortest segment whose tracks go to the worker pool: below it (tiny segments at a loop end, an
/// odd callback size) waking the workers costs more than the work.
const PARALLEL_MIN_FRAMES: usize = 32;

/// How long the dispatching thread of an offline render waits for running track jobs before it
/// looks again (offline renders have no deadline; nothing is counted as late).
const OFFLINE_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

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
            cut: None,
            bufs: Bufs::new(),
            events: EventList::new(MAX_EVENTS_PER_BLOCK),
            fx_events: EventList::new(MAX_EVENTS_PER_BLOCK),
            live: Vec::with_capacity(MAX_EVENTS_PER_BLOCK),
            live_notes: Vec::with_capacity(MAX_COMMANDS_PER_BLOCK),
            note_ids: NoteIds::default(),
            unreported: false,
            dropped: 0,
            held_input: 0,
            lanes: vec![Lane::new()].into_boxed_slice(),
            pool: None,
            order: vec![0; MAX_TRACKS].into_boxed_slice(),
            pos: vec![0; MAX_TRACKS].into_boxed_slice(),
            missing: (0..MAX_TRACKS).map(|_| AtomicU16::new(0)).collect(),
            exclusive: true,
            routed: false,
            gates: None,
            edges: (0..MAX_LINES).map(|_| EdgeState::new()).collect(),
            side: stereo(),
            stats: Arc::new(WorkerStats::default()),
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
        core.finish_cut();
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

    /// Non-RT (engine out of the audio callback). Processes tracks in parallel from now on: starts
    /// `threads` worker threads (at most [`MAX_WORKERS`]; 0 = none, every track on the calling
    /// thread) at `SCHED_FIFO` `priority` (`None`: normal scheduling), replacing the running pool
    /// (whose threads are joined). Keeps the pool if it already has that many threads and that
    /// priority. If no thread can start, the engine stays single-threaded; the status says so.
    ///
    /// The output does not change: it is bit-exact with the single-threaded engine (see
    /// [`crate::workers`]). The app starts the pool when a stream starts; export uses it too.
    pub fn set_workers(&mut self, threads: usize, priority: Option<i32>) -> WorkerStatus {
        let core = &mut self.core;
        let threads = threads.min(MAX_WORKERS);
        if let Some(pool) = &core.pool
            && pool.threads() == threads
            && pool.status().priority == priority
        {
            return pool.status().clone();
        }
        core.pool = None;
        if threads == 0 {
            core.lanes = vec![Lane::new()].into_boxed_slice();
            return WorkerStatus::default();
        }
        let (pool, status) = Pool::start(threads, priority, Arc::clone(&core.stats));
        let lanes = pool.as_ref().map_or(0, Pool::threads) + 1;
        core.lanes = (0..lanes).map(|_| Lane::new()).collect();
        core.pool = pool;
        status
    }

    /// Worker threads running (0: single-threaded).
    pub fn workers(&self) -> usize {
        self.core.pool.as_ref().map_or(0, Pool::threads)
    }

    /// Non-RT. The project latency of `program` in frames: the delay at which a note's sound
    /// reaches the output, `out(master)` of ARCHITECTURE §6.8.5. With a routing graph it is
    /// [`Routing::latency`] (every path lined up at the master); in a flat program (which has no
    /// routing latency) it is the longest track chain, the one the other tracks are delayed to.
    /// The master's active effects come on top. Chain latencies are read from the processors in
    /// their slots (a failed or missing one adds none), so they are the ones at the current rate.
    /// The export skips this many frames so the file lines up with the song timeline.
    pub fn project_latency(&self, program: &Program) -> u64 {
        let chain = |chain: &[ChainSlot]| -> u64 {
            chain
                .iter()
                .filter(|s| !s.bypass)
                .filter_map(|s| self.core.slots.get(s.slot.0 as usize))
                .filter(|slot| !slot.failed)
                .filter_map(|slot| slot.processor.as_ref())
                .map(|p| u64::from(p.latency()))
                .sum()
        };
        let tracks = if program.routing.is_flat() {
            program
                .tracks
                .iter()
                .map(|t| chain(&t.chain))
                .max()
                .unwrap_or(0)
        } else {
            u64::from(program.routing.latency())
        };
        tracks + chain(&program.master.chain)
    }

    /// The worker pool's counters (shared; they survive [`Engine::set_workers`]).
    pub fn worker_stats(&self) -> Arc<WorkerStats> {
        Arc::clone(&self.core.stats)
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

/// Non-RT. A zeroed stereo buffer of `MAX_BLOCK` frames.
fn stereo() -> [Box<[f32]>; 2] {
    [
        vec![0.0; MAX_BLOCK].into_boxed_slice(),
        vec![0.0; MAX_BLOCK].into_boxed_slice(),
    ]
}

/// Runtime state of one track or bus (a node of the routing graph), kept across program swaps by
/// track id.
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
    /// The instrument's events of the current segment (capacity `MAX_EVENTS_PER_BLOCK`).
    events: Vec<Event>,
    /// The track's output of the current segment (after fader, balance and delay compensation),
    /// left and right, summed into the master bus in track order or into its bus.
    out: [Box<[f32]>; 2],
    /// The pre-fader tap of the current segment, when a send or sidechain reads it (WP-102).
    pre: [Box<[f32]>; 2],
    /// The post-fader tap (fader, pan, mute; not solo) of the current segment, when read.
    post: [Box<[f32]>; 2],
    /// The fader of the post tap (the main fader also closes for solo).
    post_fader: Fader,
    /// The post tap was made in the last segment (else its fader starts at its target).
    post_on: bool,
    /// A processor of the chain failed in the current segment.
    failed: bool,
    /// The MIDI effect stage: the note guard and the signature of the installed chain (WP-88).
    stage: StageState,
    /// Events the stage dropped in the current segment (counted in the sum phase).
    stage_dropped: u32,
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
            events: Vec::with_capacity(MAX_EVENTS_PER_BLOCK),
            out: stereo(),
            pre: stereo(),
            post: stereo(),
            post_fader: Fader::at((0.0, 0.0)),
            post_on: false,
            failed: false,
            stage: StageState::new(),
            stage_dropped: 0,
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
/// Length of the fade before a hard cut resets the processors, in seconds.
const CUT_SECONDS: f64 = 0.005;

/// A hard cut in progress: `pos` of `len` frames of the fade are done.
#[derive(Clone, Copy)]
struct Cut {
    pos: u32,
    len: u32,
}

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
    /// The fade of a hard cut in progress (`Command::Stop` while stopped).
    cut: Option<Cut>,
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
    /// Keys held on the MIDI inputs (bit = key), published as [`Telemetry::held_input_keys`].
    held_input: u128,
    /// Scratch of the threads that run track chains: lane 0 for the calling thread, lane `w` for
    /// worker `w`.
    lanes: Box<[Lane]>,
    /// The worker pool, if any ([`Engine::set_workers`]).
    pool: Option<Pool>,
    /// Jobs of a segment: job `j` processes program track `order[j]`; lower jobs are claimed
    /// first when ready.
    order: Box<[u16]>,
    /// The job of each program track (`pos[order[j]] == j`).
    pos: Box<[u16]>,
    /// Per program track: inputs not done yet in the running segment (routing graph).
    missing: Box<[AtomicU16]>,
    /// No slot appears in two track chains of the program, so the chains may run in parallel.
    exclusive: bool,
    /// The program has a routing graph that fits its tracks (else: flat).
    routed: bool,
    /// Fast-path gate table (`Command::SetGates`) until the next program.
    gates: Option<GateTable>,
    /// Edge states of sends, sidechains and head delays, by `LineId`.
    edges: Box<[EdgeState]>,
    /// The sidechain input of the master's devices.
    side: [Box<[f32]>; 2],
    stats: Arc<WorkerStats>,
}

/// Scratch buffers of one thread that runs track chains.
struct Lane {
    bufs: Bufs,
    fx_events: EventList,
    stage: StageLists,
    /// The sidechain input of the device being processed.
    side: [Box<[f32]>; 2],
}

impl Lane {
    /// Non-RT.
    fn new() -> Lane {
        Lane {
            bufs: Bufs::new(),
            fx_events: EventList::new(MAX_EVENTS_PER_BLOCK),
            stage: StageLists::new(MAX_EVENTS_PER_BLOCK),
            side: stereo(),
        }
    }
}

/// One segment's jobs (a bus's input sum, chain, fader, balance, meter, taps, delay compensation
/// into the track's `out`), shared by the threads that run them. Job `j` processes program track
/// `order[j]`; with routing it may run once the jobs it reads are done (`missing`).
struct TrackJobs<'a> {
    tracks: &'a [Arc<TrackProgram>],
    map: &'a [u16],
    order: &'a [u16],
    pos: &'a [u16],
    missing: &'a [AtomicU16],
    states: *mut TrackState,
    slots: SharedSlots,
    lanes: *mut Lane,
    lane_count: usize,
    edges: Edges,
    routing: &'a Routing,
    /// The program's routing applies (else every track is a flat source into the master).
    routed: bool,
    /// Mute and solo by table; `None`: the flat rule with `any_solo`.
    gates: Option<&'a GateTable>,
    ctx: &'a ProcessContext,
    any_solo: bool,
    n: usize,
}

// SAFETY: the raw pointers are only dereferenced in `TrackJobs::run`, whose contract keeps the
// track states, slots, edge states and lanes of concurrent jobs disjoint and reads other jobs'
// states only after they are done; everything else is shared read-only (`TrackProgram`,
// `Routing` and `ProcessContext` are `Sync`).
unsafe impl Sync for TrackJobs<'_> {}

/// The engine's edge states, seen by the jobs: each line is used by one job (the compiler gives
/// every send, sidechain and head delay its own line).
#[derive(Clone, Copy)]
struct Edges {
    ptr: *mut EdgeState,
    len: usize,
}

impl Edges {
    /// # Safety
    /// No other reference to line `delay.line` exists while the result is used.
    unsafe fn get<'e>(self, delay: Delay) -> &'e mut EdgeState {
        let index = usize::from(delay.line.0);
        assert!(index < self.len);
        // SAFETY: in bounds; exclusive per the caller.
        unsafe { &mut *self.ptr.add(index) }
    }
}

/// Publishes a finished job's dependents (also when it unwinds, so the batch always ends).
struct Release<'j, 'p> {
    jobs: &'j TrackJobs<'j>,
    route: NodeRoute,
    publisher: &'j Publisher<'p>,
}

impl Drop for Release<'_, '_> {
    fn drop(&mut self) {
        let mut ready = 0u128;
        for &d in self.jobs.routing.dependents(self.route.dependents) {
            let d = usize::from(d);
            if self.jobs.missing[d].fetch_sub(1, Ordering::AcqRel) == 1 {
                ready |= 1 << self.jobs.pos[d];
            }
        }
        self.publisher.publish(ready);
    }
}

/// The routing a chain sees while it runs: its devices' sidechain inputs and a MIDI track's head
/// delay.
struct Hook<'h> {
    sidechains: &'h [SidechainRoute],
    head: Option<Delay>,
    /// Finished nodes (sidechain sources), read only.
    states: *const TrackState,
    map: &'h [u16],
    edges: Edges,
    side: &'h mut [Box<[f32]>; 2],
    n: usize,
}

impl ChainHook for Hook<'_> {
    fn head(&mut self, l: &mut [f32], r: &mut [f32]) {
        if let Some(delay) = self.head {
            // SAFETY: the head delay's line belongs to this job's track.
            let edge = unsafe { self.edges.get(delay) };
            edge.line.process(l, r, delay.frames);
        }
    }

    fn sidechain(&mut self, index: usize) -> Option<[&[f32]; 2]> {
        let route = self
            .sidechains
            .iter()
            .find(|s| usize::from(s.device) == index)?;
        // SAFETY: the source is another node whose job is done (it is an input of this one, or
        // every job is done for the master); nobody writes it until the next segment.
        let source = unsafe {
            &*self
                .states
                .add(usize::from(self.map[usize::from(route.from)]))
        };
        let tap = match route.tap {
            Tap::Pre => &source.pre,
            Tap::Post => &source.post,
        };
        // SAFETY: the sidechain's line belongs to its holder, which runs this chain.
        let edge = unsafe { self.edges.get(route.delay) };
        let n = self.n;
        let [l, r] = &mut *self.side;
        edge.delayed(
            [&mut l[..n], &mut r[..n]],
            [&tap[0][..n], &tap[1][..n]],
            route.delay.frames,
        );
        Some([&l[..n], &r[..n]])
    }
}

impl TrackJobs<'_> {
    /// RT. Runs job `j` on `lane`; publishes the jobs that become ready with it.
    ///
    /// # Safety
    /// No other thread uses `lane` meanwhile, each job runs at most once per segment and only
    /// after the jobs it reads (routing), nothing else touches the track states, slots, edge
    /// states and lanes while jobs run, and the program's track chains are slot-exclusive if jobs
    /// run concurrently.
    unsafe fn run(&self, j: usize, lane: usize, publisher: &Publisher<'_>) {
        let index = usize::from(self.order[j]);
        let route = if self.routed {
            self.routing.node(index)
        } else {
            NodeRoute::default()
        };
        let _release = Release {
            jobs: self,
            route,
            publisher,
        };
        let track = &*self.tracks[index];
        let state = usize::from(self.map[index]);
        assert!(state < MAX_TRACKS && lane < self.lane_count);
        // SAFETY: `map` is injective (one state per program track) and every job runs once, so
        // no other job reaches this state; the lane belongs to the calling thread.
        let (state, lane) = unsafe { (&mut *self.states.add(state), &mut *self.lanes.add(lane)) };
        let n = self.n;
        let Lane {
            bufs,
            fx_events,
            stage: stage_lists,
            side,
        } = lane;
        bufs.a[0][..n].fill(0.0);
        bufs.a[1][..n].fill(0.0);
        if route.bus {
            self.sum_inputs(route, bufs);
        }
        let mut slots = self.slots;
        // The MIDI effect stage rewrites the track's events for the instrument (a track without
        // MIDI effects and without notes left over from some goes straight on).
        let stages = track.stage_count();
        let events: &[Event] = if stages > 0 || state.stage.holds_notes() {
            let (events, dropped) = stage::run(
                &track.chain,
                stages,
                &mut slots,
                self.ctx,
                &state.events,
                &mut state.stage,
                stage_lists,
            );
            state.stage_dropped = dropped;
            events
        } else {
            state.stage_dropped = 0;
            &state.events
        };
        let mut hook = Hook {
            sidechains: self.routing.sidechains(route.sidechains),
            head: route.head,
            states: self.states,
            map: self.map,
            edges: self.edges,
            side,
            n,
        };
        let (result, failed) = run_chain(
            &track.chain,
            &mut slots,
            self.ctx,
            events,
            fx_events,
            bufs,
            n,
            !route.bus,
            &mut hook,
        );
        state.failed = failed;
        let gain = state.gain.unwrap_or(track.gain);
        let pan = state.pan.unwrap_or(track.pan);
        let (open, muted) = match self.gates {
            Some(gates) => (gates.output_open(index), gates.muted(index)),
            None => {
                let mute = state.mute.unwrap_or(track.mute);
                let solo = state.solo.unwrap_or(track.solo);
                (audible(mute, solo, self.any_solo), mute)
            }
        };
        let (l, r) = bufs.pair(result, n);
        if route.pre {
            state.pre[0][..n].copy_from_slice(l);
            state.pre[1][..n].copy_from_slice(r);
        }
        if route.post {
            let target = targets(gain, pan, !muted);
            if !state.post_on {
                state.post_fader = Fader::at(target);
            }
            let [pl, pr] = &mut state.post;
            pl[..n].copy_from_slice(l);
            pr[..n].copy_from_slice(r);
            state.post_fader.apply(&mut pl[..n], &mut pr[..n], target);
        }
        state.post_on = route.post;
        state.fader.apply(l, r, targets(gain, pan, open));
        track.meter.record(peak(l), peak(r));
        state.pdc.process(l, r, track.pdc_delay);
        state.out[0][..n].copy_from_slice(l);
        state.out[1][..n].copy_from_slice(r);
    }

    /// RT. A bus's input sum into `bufs.a` (zeroed): its sources' outputs and sends, in the
    /// program's order. `bufs.b` is scratch.
    fn sum_inputs(&self, route: NodeRoute, bufs: &mut Bufs) {
        let n = self.n;
        let Bufs { a, b, .. } = bufs;
        let [al, ar] = a;
        let [bl, br] = b;
        for input in self.routing.inputs(route.inputs) {
            // SAFETY: an input's job is done before this one runs; nobody writes it meanwhile.
            let source = unsafe {
                &*self
                    .states
                    .add(usize::from(self.map[usize::from(input.from)]))
            };
            let Some(send) = input.send else {
                add(&mut al[..n], &source.out[0][..n]);
                add(&mut ar[..n], &source.out[1][..n]);
                continue;
            };
            let tap = match send.tap {
                Tap::Pre => &source.pre,
                Tap::Post => &source.post,
            };
            let open = self
                .gates
                .is_none_or(|g| g.send_open(usize::from(send.gate)));
            // SAFETY: the send's line belongs to its target bus, which runs this job.
            let edge = unsafe { self.edges.get(send.delay) };
            edge.add_send(
                [&mut al[..n], &mut ar[..n]],
                [&tap[0][..n], &tap[1][..n]],
                [&mut bl[..n], &mut br[..n]],
                send.delay.frames,
                if open { send.gain } else { 0.0 },
            );
        }
    }
}

/// RT. A track's stage signature changed from `state.stage.sig` to `new`: the instrument's sounding
/// notes get note-offs at the next call, whichever table holds them.
fn restage(state: &mut TrackState, new: StageSig) {
    let old = state.stage.sig;
    if old.stages == 0
        && new.stages > 0
        && let Some(instrument) = old.instrument
    {
        // Stages now stand in front of an instrument that holds sequencer notes: the guard owns
        // their note-offs from here on.
        let stage = &mut state.stage;
        state
            .seq
            .drain_slot(instrument, |key, channel, id| stage.adopt(key, channel, id));
    }
    if let Some(head) = old.head()
        && new.head() != Some(head)
    {
        // Notes that entered the old stage chain were tagged with its first slot.
        state.seq.forget_slot(head);
    }
    state.stage.flush_all();
    state.stage.sig = new;
}

/// Whether a command retires an object (and therefore needs room in the garbage holding list).
fn retires(command: &Command) -> bool {
    matches!(
        command,
        Command::SetProgram(_) | Command::InsertProcessor { .. } | Command::RemoveProcessor { .. }
    )
}

/// The held-keys bitmap after the MIDI message `data`: a note-on sets its key, a note-off (or a
/// note-on with velocity 0) clears it, "all notes off" / "all sound off" clear everything. All
/// ports and channels merge into one keyboard.
fn held_after(held: u128, data: [u8; 3]) -> u128 {
    let [status, data1, data2] = data;
    let bit = 1u128 << (data1 & 0x7f);
    match status & 0xf0 {
        0x90 if data2 > 0 => held | bit,
        0x80 | 0x90 => held & !bit,
        0xb0 if data1 == 120 || data1 == 123 => 0,
        _ => held,
    }
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
        // Taken once per callback; every segment and processor shares it.
        let entered = std::time::Instant::now();
        self.outbox.flush();
        self.drain_commands();
        if self.unreported {
            self.report_failures();
        }
        let n = out_l.len().min(out_r.len());
        let rate = f64::from(self.sample_rate);
        let deadline = (!offline).then(|| {
            entered + std::time::Duration::from_secs_f64(n as f64 / rate * DEADLINE_SHARE)
        });
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
                deadline,
            );
            if let Some(to) = self.transport.advance(len, end, &self.song, rate) {
                if self.transport.state == TransportState::Recording {
                    // Every lap is its own take: keys held across the wrap end at the loop end.
                    self.close_captures(self.song.loop_range.end);
                }
                let count = self.track_count();
                for &state in &self.map[..count] {
                    let seq = &mut self.tracks[usize::from(state)].seq;
                    seq.flush(false);
                    seq.chase();
                }
                self.send(EngineEvent::LoopWrapped { to });
            }
            done = seg_end;
        }
        self.apply_cut(&mut out_l[..n], &mut out_r[..n]);
        out_l[n..].fill(0.0);
        out_r[n..].fill(0.0);
        self.live_notes.clear();
        if self.unreported {
            self.report_failures();
        }
        let held = midi
            .iter()
            .fold(self.held_input, |held, m| held_after(held, m.data));
        let t = &self.telemetry;
        if held != self.held_input {
            self.held_input = held;
            t.held_input[0].store(held as u64, Ordering::Relaxed);
            t.held_input[1].store((held >> 64) as u64, Ordering::Relaxed);
        }
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
                self.finish_cut();
                if !self.transport.playing() {
                    self.start(TransportState::Playing);
                }
            }
            Command::Stop => {
                // A second Stop (already stopped) is the hard cut: reset, tails end at once.
                let hard = !self.transport.playing();
                self.close_captures(self.transport.tick(&self.song, rate));
                self.transport.state = TransportState::Stopped;
                if hard {
                    self.begin_cut();
                } else {
                    self.silence(false);
                }
            }
            Command::Locate(tick) => {
                if self.transport.playing() {
                    self.close_captures(self.transport.tick(&self.song, rate));
                    self.silence(false);
                    self.chase();
                }
                let tick = tick.max(Tick::ZERO);
                self.transport.pos = self.song.map.tick_to_sample(tick, rate);
                self.transport.wrapped = false;
            }
            Command::Record { from } => match self.transport.state {
                TransportState::Stopped => {
                    self.finish_cut();
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
            Command::SetProgram(program) => {
                // The cut resets the processors of the program it fades out.
                self.finish_cut();
                self.install(program);
            }
            Command::InsertProcessor { slot, processor } => {
                self.finish_cut();
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
                self.finish_cut();
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
            Command::SetGates(gates) => self.gates = Some(gates),
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
        let tracks = self
            .program
            .as_deref()
            .map_or(&[][..], |p| &p.tracks[..count]);
        for (track, &index) in tracks.iter().zip(&self.map[..count]) {
            let state = &mut self.tracks[usize::from(index)];
            state.seq.forget_slot(slot);
            // A replaced or removed MIDI effect or instrument: what the instrument got through the
            // stage ends.
            let stages = track.stage_count();
            if track.chain.iter().take(stages + 1).any(|d| d.slot == slot) {
                state.stage.flush_all();
            }
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
            state.seq.chase();
        }
    }

    /// RT. Every track chases its notes in its next call (playback start, locate, loop wrap).
    fn chase(&mut self) {
        let count = self.track_count();
        for &index in &self.map[..count] {
            self.tracks[usize::from(index)].seq.chase();
        }
    }

    /// RT. Starts the hard cut: note-offs and `AllNotesOff` now, the output fades to silence over
    /// [`CUT_SECONDS`] and every processor is reset when the fade is over. A cut already running
    /// goes on unchanged.
    fn begin_cut(&mut self) {
        if self.cut.is_some() {
            return;
        }
        let len = (CUT_SECONDS * f64::from(self.sample_rate)).round().max(1.0) as u32;
        self.cut = Some(Cut { pos: 0, len });
        self.silence(false);
    }

    /// RT. Ends a running hard cut at once: resets every processor and gives the output its full
    /// level back (the processors are silent after the reset). No-op without a cut.
    fn finish_cut(&mut self) {
        if self.cut.take().is_some() {
            self.silence(true);
        }
    }

    /// RT. Applies the cut's gain ramp to the final output of this call (after master and
    /// metronome) and ends the cut once the ramp is over: the rest of the call is silent, the
    /// processors are reset after it.
    fn apply_cut(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let Some(cut) = self.cut.as_mut() else {
            return;
        };
        let inv = 1.0 / cut.len as f32;
        for (l, r) in out_l.iter_mut().zip(out_r.iter_mut()) {
            let gain = if cut.pos < cut.len {
                1.0 - cut.pos as f32 * inv
            } else {
                0.0
            };
            cut.pos = cut.pos.saturating_add(1);
            *l *= gain;
            *r *= gain;
        }
        if cut.pos >= cut.len {
            self.finish_cut();
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
            state.stage.clear();
            state.stage.sig = StageSig::of(&track.chain);
            state.seq.reseed(track_seed(self.seed, track.id));
            state.clear_fast_path();
            let open = match &program.routing.gates {
                Some(gates) => gates.output_open(i),
                None => audible(track.mute, track.solo, any_solo),
            };
            state.fader = Fader::at(targets(track.gain, track.pan, open));
            state.post_on = false;
        }
        for (index, state) in self.tracks.iter_mut().enumerate() {
            if !used[index] {
                state.id = None;
            }
        }
        self.map[..count].copy_from_slice(&map[..count]);
        self.exclusive = chains_exclusive(program.tracks[..count].iter().map(|t| &t.chain[..]));
        // The routing graph, if it fits the program (the compiler always makes one that does).
        let routing = &*program.routing;
        self.routed =
            !routing.is_flat() && routing.order.len() == count && routing.nodes.len() == count;
        self.gates = None;
        // Edges keep their delay lines and gains across the swap; a line that changed hands
        // starts over.
        for &(key, line) in program.routing.lines.entries() {
            if let Some(edge) = self.edges.get_mut(usize::from(line.0)) {
                edge.assign(key);
            }
        }
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
        // A track whose MIDI effect stage changed (effect added, removed, bypassed, reordered, or the
        // instrument replaced) loses the notes it sounds: the instrument's end at once, the stages
        // are reset.
        for (track, &state) in program.tracks[..count].iter().zip(&self.map[..count]) {
            let sig = StageSig::of(&track.chain);
            let state = &mut self.tracks[usize::from(state)];
            if state.stage.sig == sig {
                continue;
            }
            restage(state, sig);
            for device in &track.chain[..usize::from(sig.stages)] {
                if let Some(processor) = self
                    .slots
                    .get_mut(device.slot.0 as usize)
                    .and_then(|s| s.processor.as_mut())
                {
                    processor.reset();
                }
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
    /// starts at frame `frame0` of the call. `deadline` is the callback's (`run` sets one exactly
    /// for realtime calls, so `None` means offline).
    fn segment(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        midi: &[MidiInEvent],
        frame0: usize,
        latency: u32,
        deadline: Option<std::time::Instant>,
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
            offline: deadline.is_none(),
            deadline,
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
            lanes,
            pool,
            order,
            pos,
            missing,
            exclusive,
            routed,
            gates,
            edges,
            side,
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
                let routing = &*program.routing;
                let routed = *routed;
                // 1. Every track's events, on this thread in track order (note ids, probability
                // rolls and captured input stay deterministic). A bridge instrument at the head of
                // a chain starts at once (`Processor::begin`), so it works while the chains run;
                // not one with a sidechain, whose input exists only when its source is done.
                let mut kicked = [false; MAX_TRACKS];
                let tracks_and_states = program.tracks[..count].iter().zip(&map[..count]);
                for (index, (track, &state)) in tracks_and_states.enumerate() {
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
                    // An armed track monitors on its own ("Auto", owner decision 2026-11-15); I
                    // monitors without arming.
                    let play = track.monitor || track.arm;
                    if play || capture {
                        for m in midi.iter().filter(|m| accepts(&track.input, m)) {
                            if live.len() == live.capacity() {
                                *dropped += 1;
                                break;
                            }
                            let frame = (m.frame as usize).clamp(frame0, frame0 + n - 1);
                            live.push(LiveMsg {
                                offset: (frame - frame0) as u32,
                                data: m.data,
                                play,
                                capture,
                            });
                        }
                    }
                    // The instrument's events. On a track with MIDI effects, the sequencer's and
                    // the live notes enter the stage chain instead (tagged with the first stage's
                    // slot), and the track's job (`stage::run`) makes the instrument's events.
                    let staged = track.stage_count() > 0;
                    let first = track.instrument();
                    let instrument = first
                        .filter(|d| {
                            slots
                                .get(d.slot.0 as usize)
                                .is_some_and(|s| s.runs(d.bypass))
                        })
                        .map(|d| d.slot);
                    let target = if staged {
                        track.chain.first().map(|d| d.slot)
                    } else {
                        instrument
                    };
                    events.clear();
                    state.seq.flushed_offs(target, events);
                    if !staged
                        && let Some(device) = first
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
                        target,
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
                    state.events.clear();
                    state.events.extend_from_slice(events.finish());
                    let sidechained = routed
                        && routing
                            .sidechains(routing.node(index).sidechains)
                            .iter()
                            .any(|s| usize::from(s.device) == track.stage_count());
                    if !staged
                        && !sidechained
                        && let Some(slot) = instrument
                        && let Some(processor) = slots
                            .get_mut(slot.0 as usize)
                            .and_then(|s| s.processor.as_mut())
                    {
                        let silence = &bufs.silence[..n];
                        kicked[index] = processor.begin(&ctx, &state.events, [silence, silence]);
                    }
                }
                // 2. The jobs (bus sums, chains, fader, balance, meter, taps, delay compensation
                // into each track's `out`): on the worker pool if there is one, no slot is in two
                // chains and the segment is not tiny, else here. A job may start when the jobs it
                // reads are done; among ready jobs the earlier in the routing's order first, kicked
                // tracks last: their bridges work meanwhile.
                let job_order: &[u16] = if routed { &routing.order } else { &[] };
                let mut next = 0;
                for late in [false, true] {
                    let indices = (0..count).map(|i| match job_order.get(i) {
                        Some(&index) => usize::from(index),
                        None => i,
                    });
                    for index in indices.filter(|&i| kicked[i] == late) {
                        order[next] = index as u16;
                        pos[index] = next as u16;
                        next += 1;
                    }
                }
                let mut ready = 0u128;
                for j in 0..count {
                    let index = usize::from(order[j]);
                    let waits = if routed { routing.node(index).waits } else { 0 };
                    missing[index].store(waits, Ordering::Relaxed);
                    if waits == 0 {
                        ready |= 1 << j;
                    }
                }
                let gate_table = gates.as_ref().or(routing.gates.as_ref());
                let jobs = TrackJobs {
                    tracks: &program.tracks[..count],
                    map: &map[..count],
                    order: &order[..count],
                    pos: &pos[..count],
                    missing: &missing[..count],
                    states: tracks.as_mut_ptr(),
                    // SAFETY: the slots are only reached through `jobs` until the jobs are done;
                    // concurrent jobs only when the chains are exclusive (checked below).
                    slots: unsafe { SharedSlots::new(slots) },
                    lanes: lanes.as_mut_ptr(),
                    lane_count: lanes.len(),
                    edges: Edges {
                        ptr: edges.as_mut_ptr(),
                        len: edges.len(),
                    },
                    routing,
                    routed,
                    gates: gate_table,
                    ctx: &ctx,
                    any_solo,
                    n,
                };
                // SAFETY: `Pool::run_graph` gives every thread its own lane (0 here, 1 ..= threads
                // for the workers, all < `lanes.len()`) and claims every job once, when it was
                // published as ready (its inputs done); the chains are exclusive when it runs;
                // nothing else touches the states, slots, edges and lanes until it returns.
                // `run_serial` runs one job at a time on lane 0, also only when ready.
                let job = |j: usize, lane: usize, publisher: &Publisher<'_>| unsafe {
                    jobs.run(j, lane, publisher)
                };
                match pool.as_mut() {
                    Some(pool) if *exclusive && count > 1 && n >= PARALLEL_MIN_FRAMES => {
                        // Waits for running jobs in slices of one period (counted when exceeded).
                        let period = if deadline.is_some() {
                            std::time::Duration::from_secs_f64(n as f64 / rate)
                        } else {
                            OFFLINE_WAIT
                        };
                        pool.run_graph(count, ready, &job, period);
                    }
                    _ => run_serial(count, ready, &job),
                }
                // 3. The master bus: what outputs to it, summed in a fixed order (track order
                // without routing), whichever thread ran the jobs.
                for &state in &map[..count] {
                    let state = &tracks[usize::from(state)];
                    *unreported |= state.failed;
                    *dropped += state.stage_dropped;
                }
                let [bus_l, bus_r] = &mut bufs.bus;
                let mut sum = |from: usize| {
                    let state = &tracks[usize::from(map[from])];
                    add(&mut bus_l[..n], &state.out[0][..n]);
                    add(&mut bus_r[..n], &state.out[1][..n]);
                };
                if routed {
                    for input in routing.inputs(routing.master.inputs) {
                        sum(usize::from(input.from));
                    }
                } else {
                    (0..count).for_each(sum);
                }
                // Master inserts, fader, meter, output.
                let Bufs { a, bus, .. } = &mut *bufs;
                a[0][..n].copy_from_slice(&bus[0][..n]);
                a[1][..n].copy_from_slice(&bus[1][..n]);
                let master = &program.master;
                let mut hook = Hook {
                    sidechains: if routed {
                        routing.sidechains(routing.master.sidechains)
                    } else {
                        &[]
                    },
                    head: None,
                    states: tracks.as_ptr(),
                    map: &map[..count],
                    edges: Edges {
                        ptr: edges.as_mut_ptr(),
                        len: edges.len(),
                    },
                    side,
                    n,
                };
                let (result, failed) = run_chain(
                    &master.chain,
                    &mut **slots,
                    &ctx,
                    &[],
                    fx_events,
                    bufs,
                    n,
                    false,
                    &mut hook,
                );
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

// The engine moves into the audio callback; track jobs share the program and the context.
const _: fn() = || {
    fn send<T: Send>() {}
    fn sync<T: Sync>() {}
    send::<Engine>();
    send::<TrackState>();
    send::<Slot>();
    sync::<TrackProgram>();
    sync::<ProcessContext>();
};
