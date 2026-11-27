//! The engine-side stand-in for a plugin running in a bridge process (ARCHITECTURE §8.1).
//!
//! Implements `komponist_engine::Processor` on top of `komponist_ipc::BridgeClient`. Per call:
//! convert the events to `WireEvent`s and the transport to `WireTransport`, `begin_block`, then
//! `wait_done` until the callback's shared deadline (`ProcessContext::deadline`: 60 % of the
//! callback period after the callback started, the same for every processor of the callback).
//! The engine may split the two halves (`Processor::begin`, WP-65): for an instrument at the
//! head of a chain the block starts before the engine runs any chain, and `process` only waits
//! for it. A device whose sidechain is set gets `Processor::process_sidechain` instead (#6): the
//! sidechain travels in the block's extra shared-memory channels into the plugin's sidechain port;
//! a plain `process` sends none, so the port gets silence. Fallbacks (passthrough for effects,
//! silence for instruments): while the bridge loads (`Loading`), when a block is late or the bridge
//! busy (counted as "late"), and permanently once the bridge died (then `ProcessStatus::Failed`).
//! Undelivered note-offs are kept and sent with the next block that reaches the bridge.
//!
//! Created only by [`crate::PluginManager`]; lives on the audio thread; dropped on the UI thread
//! (engine garbage).
//!
//! Without a deadline in the context (calls outside the engine) the processor takes
//! [`DEADLINE_SHARE`] of its own call's period from its entry. Offline calls (export) wait up to
//! [`OFFLINE_DEADLINE`] instead, so a render never drops a block for timing reasons.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use komponist_engine::processor::{EventProcessor, EventSink, ParameterEdit, ParameterEditKind};
use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
    TransportInfo,
};
use komponist_ipc::shm::{
    BLOCK_FLAG_EFFECT_DETECTOR, BLOCK_FLAG_EFFECT_METER, BLOCK_FLAG_OFFLINE, BLOCK_FLAG_RESET,
    INPUT_EVENT_CAPACITY, MAX_BLOCK, OUTPUT_EVENT_CAPACITY, TRANSPORT_LOOPING, TRANSPORT_PLAYING,
    TRANSPORT_RECORDING,
};
use komponist_ipc::{
    BlockError, BlockSpec, BlockStatus, BridgeClient, BridgeMonitor, WireEvent, WireEventKind,
    WireTransport,
};
use komponist_model::{DeviceRole, PPQ};

use crate::bundled_effects;
use crate::manager::ProcessorSetup;

/// Share of the callback period by which bridges must be done (ARCHITECTURE §8.1); the engine's
/// constant, re-exported.
pub use komponist_engine::processor::DEADLINE_SHARE;

/// How long an offline call (export) waits for the bridge.
pub const OFFLINE_DEADLINE: Duration = Duration::from_secs(2);

/// Undelivered events (note-offs, parameter values) kept for the next block that reaches the
/// bridge; beyond it an `AllNotesOff` goes out instead of the note-offs.
const PENDING_CAPACITY: usize = 256;

/// [`Shared::run`]: bridge starting or plugin loading; fallback, `Continue`.
pub(crate) const RUN_LOADING: u8 = 0;
/// [`Shared::run`]: the bridge is active; blocks go through the shared memory.
pub(crate) const RUN_ACTIVE: u8 = 1;
/// [`Shared::run`]: the bridge died, hangs or failed to load; fallback, `Failed`.
pub(crate) const RUN_DEAD: u8 = 2;

/// State the plugin manager (UI thread) and the processor (audio thread) share. Only atomics are
/// touched on the audio thread; the mutex is used by `activate` (non-RT) and the manager.
#[derive(Debug)]
pub(crate) struct Shared {
    /// `RUN_*`, written by the manager.
    pub run: AtomicU8,
    /// Set by [`ExternalProcessor::activate`] when the rate or block size changed, cleared by the
    /// manager once the bridge runs with the new setup. Fallback meanwhile.
    pub reactivating: AtomicBool,
    /// The setup `activate` asked for; taken by the manager.
    pub reactivate: Mutex<Option<ProcessorSetup>>,
    /// Blocks that missed their deadline or found the bridge busy.
    pub late_blocks: AtomicU64,
    pub signal_taps: std::sync::OnceLock<crate::signal_taps::SignalTaps>,
}

impl Shared {
    pub(crate) fn new() -> Shared {
        Shared {
            run: AtomicU8::new(RUN_LOADING),
            reactivating: AtomicBool::new(false),
            reactivate: Mutex::new(None),
            late_blocks: AtomicU64::new(0),
            signal_taps: std::sync::OnceLock::new(),
        }
    }
}

/// A [`Processor`] whose work happens in a bridge process.
#[derive(Debug)]
pub struct ExternalProcessor {
    client: BridgeClient,
    monitor: BridgeMonitor,
    /// Sequence of this call's completed block, used to distinguish late GUI edits.
    completed: Option<u32>,
    shared: Arc<Shared>,
    /// Instruments fall back to silence, effects to passthrough.
    role: DeviceRole,
    /// The setup the processor was last activated for.
    setup: ProcessorSetup,
    /// Running sample counter (CLAP `steady_time`).
    steady_time: u64,
    /// `BLOCK_FLAG_RESET` until a block carried it.
    reset: bool,
    /// The wire events of the current call (capacity [`INPUT_EVENT_CAPACITY`], never grows).
    wire: Vec<WireEvent>,
    /// Undelivered note-offs and parameter values, at offset 0 (capacity [`PENDING_CAPACITY`]).
    pending: Vec<WireEvent>,
    /// An `AllNotesOff` is due (undelivered, or too many pending note-offs).
    pending_all_off: bool,
    /// A call started by [`Processor::begin`], completed by the next `process`.
    begun: Option<Started>,
    /// Offline fallback blocks, including unsuccessful replies from a plugin that is still alive.
    offline_failures: u64,
    midi: Option<MidiStage>,
    effect_meter: Option<Arc<bundled_effects::Meter>>,
}

#[derive(Debug)]
struct MidiStage {
    events: Vec<Event>,
    edits: VecDeque<ParameterEdit>,
    may_have_notes: bool,
    release_due: bool,
}

impl MidiStage {
    fn new() -> Self {
        Self {
            events: Vec::with_capacity(OUTPUT_EVENT_CAPACITY),
            edits: VecDeque::with_capacity(OUTPUT_EVENT_CAPACITY),
            may_have_notes: false,
            release_due: false,
        }
    }
}

impl ExternalProcessor {
    /// The RT end of a bridge (UI thread; allocates the event buffers).
    pub(crate) fn new(
        client: BridgeClient,
        shared: Arc<Shared>,
        role: DeviceRole,
        setup: ProcessorSetup,
    ) -> ExternalProcessor {
        ExternalProcessor {
            monitor: BridgeMonitor::new(client.region().clone()),
            client,
            completed: None,
            shared,
            role,
            setup,
            steady_time: 0,
            reset: false,
            wire: Vec::with_capacity(INPUT_EVENT_CAPACITY),
            pending: Vec::with_capacity(PENDING_CAPACITY),
            pending_all_off: false,
            begun: None,
            offline_failures: 0,
            midi: (role == DeviceRole::MidiEffect).then(MidiStage::new),
            effect_meter: None,
        }
    }

    /// RT. Writes the fallback for this call.
    fn fallback(&self, io: &mut StereoIo<'_>) {
        match self.role {
            DeviceRole::Instrument => io.silence(),
            DeviceRole::Effect | DeviceRole::MidiEffect => io.passthrough(),
        }
    }

    /// RT. Keeps an undelivered event for the next block if it must not get lost.
    fn stash(&mut self, event: WireEvent) {
        match event.decode() {
            Some(WireEventKind::NoteOff { .. }) => {
                if self.pending_all_off {
                    return;
                }
                if self.pending.len() < PENDING_CAPACITY {
                    self.pending.push(WireEvent { offset: 0, ..event });
                } else {
                    self.pending_all_off = true;
                    self.pending
                        .retain(|e| !matches!(e.decode(), Some(WireEventKind::NoteOff { .. })));
                }
            }
            Some(WireEventKind::AllNotesOff) => {
                self.pending_all_off = true;
                self.pending
                    .retain(|e| !matches!(e.decode(), Some(WireEventKind::NoteOff { .. })));
            }
            Some(WireEventKind::ParamValue { id, .. }) => {
                let event = WireEvent { offset: 0, ..event };
                let same = self.pending.iter().position(|e| {
                    matches!(e.decode(), Some(WireEventKind::ParamValue { id: other, .. }) if other == id)
                });
                match same {
                    Some(i) => self.pending[i] = event,
                    None if self.pending.len() < PENDING_CAPACITY => self.pending.push(event),
                    None => {}
                }
            }
            // Late note-ons and other messages are dropped: they would sound at the wrong time.
            _ => {}
        }
    }

    /// RT. Fills `wire` with pending events (offset 0) and then current input. MIDI effects
    /// reserve capacity for the complete input so timed notes never spill into the next call.
    fn build_wire(&mut self, events: &[Event]) -> bool {
        self.wire.clear();
        let pending_room = if self.midi.is_some() {
            let Some(room) = INPUT_EVENT_CAPACITY.checked_sub(events.len()) else {
                return false;
            };
            room
        } else {
            INPUT_EVENT_CAPACITY
        };
        if self.pending_all_off && pending_room != 0 {
            self.wire
                .push(WireEvent::new(0, WireEventKind::AllNotesOff));
            self.pending_all_off = false;
        }
        let pending_count = self.pending.len().min(pending_room - self.wire.len());
        self.wire.extend(self.pending.drain(..pending_count));
        for event in events {
            let wire = to_wire(event);
            if self.wire.len() < INPUT_EVENT_CAPACITY {
                self.wire.push(wire);
            } else {
                self.stash(wire);
            }
        }
        true
    }

    /// RT. Keeps the events of a call the bridge did not get.
    fn stash_wire(&mut self) {
        // Index loop: `stash` borrows `self` mutably.
        for i in 0..self.wire.len() {
            let event = self.wire[i];
            self.stash(event);
        }
        self.wire.clear();
    }

    /// RT. Drops everything pending (the bridge starts over: loading, re-activation).
    fn forget_pending(&mut self) {
        self.pending.clear();
        self.pending_all_off = false;
    }

    /// RT. The first half of a call: status checks, wire events and `begin_block` (with the
    /// `sidechain`, if any).
    fn start(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        input: [&[f32]; 2],
        sidechain: Option<[&[f32]; 2]>,
    ) -> Started {
        let entered = Instant::now();
        let frames = input[0].len();
        self.steady_time += frames as u64;
        let outcome = |outcome| Started { frames, outcome };
        match self.shared.run.load(Ordering::Acquire) {
            RUN_ACTIVE if !self.shared.reactivating.load(Ordering::Acquire) => {}
            RUN_DEAD => return outcome(Outcome::Fallback(ProcessStatus::Failed)),
            _ => {
                if self.midi.is_none() {
                    self.pending.retain(|event| {
                        matches!(event.decode(), Some(WireEventKind::ParamValue { .. }))
                    });
                    self.pending_all_off = false;
                }
                // The engine has accepted these events for the slot. Keep the latest value
                // per parameter so a held automation point reaches the first active block.
                for event in events {
                    if matches!(event.kind, EventKind::Param { .. }) {
                        self.stash(to_wire(event));
                    }
                }
                return outcome(Outcome::Fallback(ProcessStatus::Continue));
            }
        }
        if frames == 0
            || frames > MAX_BLOCK
            || input[1].len() < frames
            || sidechain.is_some_and(|sc| sc.iter().any(|c| c.len() < frames))
        {
            return outcome(Outcome::Fallback(ProcessStatus::Continue));
        }

        if !self.build_wire(events) {
            return outcome(Outcome::Fallback(ProcessStatus::Continue));
        }
        let mut flags = 0;
        if self.reset {
            flags |= BLOCK_FLAG_RESET;
        }
        if ctx.offline {
            flags |= BLOCK_FLAG_OFFLINE;
        } else if let Some(taps) = self.shared.signal_taps.get() {
            flags |= komponist_ipc::shm::signal_tap_flags(taps.begin(frames));
        }
        if let Some(meter) = self
            .effect_meter
            .as_ref()
            .filter(|meter| meter.requests_reduction())
        {
            flags |= BLOCK_FLAG_EFFECT_METER;
            if meter.requests_detector(frames) {
                flags |= BLOCK_FLAG_EFFECT_DETECTOR;
            }
        }
        let deadline = match ctx.deadline {
            _ if ctx.offline => entered + OFFLINE_DEADLINE,
            Some(deadline) => deadline,
            None => {
                let period = frames as f64 / f64::from(ctx.sample_rate);
                entered + Duration::from_secs_f64(period * DEADLINE_SHARE)
            }
        };
        let spec = BlockSpec {
            frames: frames as u32,
            flags,
            steady_time: self.steady_time - frames as u64,
            // The bridge's work interval targets it (macOS); offline renders have no pacing.
            deadline: (!ctx.offline).then_some(deadline),
            transport: wire_transport(&ctx.transport),
        };
        let seq = match self.client.begin_block(&spec, input, sidechain, &self.wire) {
            Ok(seq) => seq,
            Err(error) => {
                // Nothing reached the bridge: keep what must not get lost.
                self.stash_wire();
                if matches!(error, BlockError::Busy | BlockError::TooManyEvents) {
                    self.shared.late_blocks.fetch_add(1, Ordering::Relaxed);
                }
                return outcome(Outcome::Fallback(ProcessStatus::Continue));
            }
        };
        self.reset = false;
        self.wire.clear();
        outcome(Outcome::Sent { seq, deadline })
    }

    /// RT. The second half of a call: wait for the bridge's output, or write the fallback.
    fn finish(
        &mut self,
        started: Started,
        mut io: Option<&mut StereoIo<'_>>,
        offline: bool,
    ) -> (ProcessStatus, bool, u32) {
        self.completed = None;
        let (status, fallback, dropped) = match started.outcome {
            Outcome::Fallback(status) => {
                if let Some(io) = io {
                    self.fallback(io);
                }
                (status, true, 0)
            }
            Outcome::Sent { seq, deadline } => {
                let result = match io.as_mut() {
                    Some(io) => {
                        let [left, right] = &mut io.output;
                        self.client
                            .wait_done(seq, deadline, [&mut **left, &mut **right])
                    }
                    None => self.client.wait_done_events(seq, deadline),
                };
                match result {
                    // PluginError/Inactive results carry their own audio fallback. Preserve
                    // output parameter edits, but only route notes from a successful block.
                    Ok(result) => {
                        self.completed = Some(seq);
                        if result.status == BlockStatus::Ok as u32
                            && let (Some(meter), Some(io)) = (&self.effect_meter, io.as_deref())
                        {
                            meter.record(io, self.client.effect_meter(seq));
                        }
                        if result.status == BlockStatus::Ok as u32
                            && let (Some(taps), Some(payload)) =
                                (self.shared.signal_taps.get(), self.client.signal_taps(seq))
                        {
                            taps.record(payload);
                        }
                        (
                            ProcessStatus::Continue,
                            result.status != BlockStatus::Ok as u32,
                            result.dropped_events,
                        )
                    }
                    Err(_) => {
                        self.shared.late_blocks.fetch_add(1, Ordering::Relaxed);
                        if let Some(io) = io {
                            self.fallback(io);
                        }
                        (ProcessStatus::Continue, true, 0)
                    }
                }
            }
        };
        if offline && fallback {
            self.offline_failures = self.offline_failures.wrapping_add(1);
        }
        (status, fallback, dropped)
    }
}

/// A call started by [`ExternalProcessor::start`].
#[derive(Debug, Clone, Copy)]
struct Started {
    /// Frames of the call.
    frames: usize,
    outcome: Outcome,
}

/// What the start of a call did.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    /// The block went to the bridge: wait for `seq` until `deadline`.
    Sent { seq: u32, deadline: Instant },
    /// No block went out: write the fallback and return the status.
    Fallback(ProcessStatus),
}

impl Processor for ExternalProcessor {
    /// Non-RT (engine out of the callback, e.g. inside `Engine::set_sample_rate`). If the rate or
    /// block size changed, asks the plugin manager (through a queue it polls) to re-activate the
    /// bridge, and returns at once; until the bridge is `Active` again the processor uses the
    /// fallback.
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(ProcessorError::UnsupportedSampleRate(sample_rate));
        }
        if max_block == 0 || max_block > MAX_BLOCK {
            return Err(ProcessorError::BlockTooLarge(max_block));
        }
        let setup = ProcessorSetup {
            sample_rate,
            max_block,
        };
        if setup == self.setup {
            return Ok(());
        }
        self.setup = setup;
        if let Some(taps) = self.shared.signal_taps.get() {
            taps.set_rate(sample_rate);
        }
        if let Some(meter) = &self.effect_meter {
            meter.set_rate(sample_rate);
        }
        self.shared.reactivating.store(true, Ordering::Release);
        *self
            .shared
            .reactivate
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(setup);
        Ok(())
    }

    /// RT: one shared-memory round trip with deadline, or the fallback. Completes a block started
    /// by [`Processor::begin`] instead of starting one.
    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let started = match self.begun.take() {
            Some(started) if started.frames == io.frames() => started,
            // Not begun, or (never from the engine) begun for another call: whatever the bridge
            // does with that block, its result is stale by sequence number.
            _ => self.start(ctx, events, io.input, None),
        };
        self.finish(started, Some(io), ctx.offline).0
    }

    /// RT: [`Processor::process`] with the device's sidechain, which goes into the plugin's
    /// sidechain port through the block's extra channels (#6). The engine never starts such a
    /// device early ([`Processor::begin`]): its sidechain exists only once its source ran.
    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: [&[f32]; 2],
    ) -> ProcessStatus {
        // A block begun without the sidechain (never from the engine) is stale by sequence
        // number once this one goes out.
        self.begun = None;
        let started = self.start(ctx, events, io.input, Some(sidechain));
        self.finish(started, Some(io), ctx.offline).0
    }

    /// RT: starts the block (wire events, `begin_block`, futex wake) so that the bridge works
    /// while the engine processes other tracks; the next `process` call waits for it.
    fn begin(&mut self, ctx: &ProcessContext, events: &[Event], input: [&[f32]; 2]) -> bool {
        self.begun = Some(self.start(ctx, events, input, None));
        true
    }

    fn pop_parameter_edit(&mut self) -> Option<ParameterEdit> {
        if let Some(edit) = self.midi.as_mut().and_then(|stage| stage.edits.pop_front()) {
            return Some(edit);
        }
        while let Some(event) = self.monitor.pop_output_event() {
            if let Some(edit) = parameter_edit(&event, self.completed) {
                return Some(edit);
            }
        }
        None
    }

    /// RT: the latency the bridge published in shared memory.
    fn latency(&self) -> u32 {
        self.client.region().latency()
    }

    fn offline_failures(&self) -> u64 {
        self.offline_failures
    }

    /// RT: sets `BLOCK_FLAG_RESET` on the next block.
    fn reset(&mut self) {
        self.reset = true;
        // The plugin silences every voice on reset; old note-offs are moot.
        self.forget_pending();
    }
    fn event_processor(&mut self) -> Option<&mut dyn EventProcessor> {
        self.midi.is_some().then_some(self)
    }
}

impl EventProcessor for ExternalProcessor {
    fn set_param(&mut self, id: u32, value: f64) {
        self.stash(WireEvent::new(0, WireEventKind::ParamValue { id, value }));
    }

    fn process_events(&mut self, ctx: &ProcessContext, input: &[Event], out: &mut EventSink<'_>) {
        static SILENCE: [f32; MAX_BLOCK] = [0.0; MAX_BLOCK];
        let frames = ctx.frames as usize;
        let Some(silence) = SILENCE.get(..frames) else {
            return;
        };
        let started = self.start(ctx, input, [silence; 2], None);
        let (_, fallback, dropped) = self.finish(started, None, ctx.offline);
        let stage = self
            .midi
            .as_mut()
            .expect("MIDI processor has stage buffers");
        stage.events.clear();

        while let Some(wire) = self.monitor.pop_output_event() {
            if let Some(mut edit) = parameter_edit(&wire, self.completed) {
                // A stage can run multiple subsegments if it has sample-accurate automation.
                edit.offset += out.block_offset();
                if stage.edits.len() < stage.edits.capacity() {
                    stage.edits.push_back(edit);
                }
            } else if !fallback
                && self.completed == Some(wire.reserved)
                && wire.offset < ctx.frames
                && let Some(kind) = midi_event_kind(&wire)
                && stage.events.len() < stage.events.capacity()
            {
                stage.events.push(Event {
                    offset: wire.offset,
                    kind,
                });
            }
        }

        // A partial result can contain a note-on whose note-off was lost. Drop the partial
        // output, reset the remote stage on the next block, and release its downstream voices
        // before allowing new note events. No room in this call leaves the release pending.
        let required = stage.events.len() + usize::from(stage.release_due);
        if fallback || dropped != 0 || required > out.room() {
            if ctx.offline && !fallback {
                self.offline_failures = self.offline_failures.wrapping_add(1);
            }
            self.reset = true;
            stage.release_due |= stage.may_have_notes;
            stage.events.clear();
        }
        if stage.release_due {
            if !out.push(0, EventKind::AllNotesOff) {
                return;
            }
            stage.release_due = false;
            stage.may_have_notes = false;
        }
        for event in &stage.events {
            out.push(event.offset, event.kind);
            match event.kind {
                EventKind::NoteOn { .. } => stage.may_have_notes = true,
                EventKind::AllNotesOff => stage.may_have_notes = false,
                _ => {}
            }
        }
    }

    fn reset(&mut self) {
        Processor::reset(self);
    }
}

fn parameter_edit(event: &WireEvent, completed: Option<u32>) -> Option<ParameterEdit> {
    let kind = match event.decode()? {
        WireEventKind::ParamValue { id, value } if value.is_finite() => {
            ParameterEditKind::Value { id, value }
        }
        WireEventKind::ParamGesture { id, begin } => ParameterEditKind::Gesture { id, begin },
        _ => return None,
    };
    // A late request's GUI edit did not sound in that block. Retain its gesture order and
    // anchor it to the next segment; note messages never use this fallback.
    let offset = if completed == Some(event.reserved) {
        event.offset
    } else {
        0
    };
    Some(ParameterEdit { offset, kind })
}

/// Decode a hosted MIDI output on the engine's one downstream note port.
pub(crate) fn midi_event_kind(event: &WireEvent) -> Option<EventKind> {
    match event.decode()? {
        WireEventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        } if channel < 16 && key < 128 && (1..128).contains(&velocity) => Some(EventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        }),
        WireEventKind::NoteOff {
            channel,
            key,
            note_id,
        } if channel < 16 && key < 128 => Some(EventKind::NoteOff {
            channel,
            key,
            note_id,
        }),
        WireEventKind::Midi([status, data1, data2])
            if (0x80..0xf0).contains(&status) && data1 < 128 && data2 < 128 =>
        {
            Some(EventKind::Midi([status, data1, data2]))
        }
        WireEventKind::AllNotesOff => Some(EventKind::AllNotesOff),
        _ => None,
    }
}

/// RT. An engine event on the wire.
pub(crate) fn to_wire(event: &Event) -> WireEvent {
    let kind = match event.kind {
        EventKind::NoteOn {
            key,
            velocity,
            channel,
            note_id,
        } => WireEventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        },
        EventKind::NoteOff {
            key,
            channel,
            note_id,
        } => WireEventKind::NoteOff {
            channel,
            key,
            note_id,
        },
        EventKind::Midi(bytes) => WireEventKind::Midi(bytes),
        EventKind::Param { id, value } => WireEventKind::ParamValue { id, value },
        EventKind::AllNotesOff => WireEventKind::AllNotesOff,
    };
    WireEvent::new(event.offset, kind)
}

/// RT. The engine's transport in quarter-note beats.
pub(crate) fn wire_transport(t: &TransportInfo) -> WireTransport {
    let quarter = PPQ as f64;
    let mut flags = 0;
    if t.playing {
        flags |= TRANSPORT_PLAYING;
    }
    if t.recording {
        flags |= TRANSPORT_RECORDING;
    }
    if t.looping {
        flags |= TRANSPORT_LOOPING;
    }
    let bar_ticks = t.meter.ticks_per_bar().max(1) as f64;
    let bar = (t.position_ticks / bar_ticks).floor();
    WireTransport {
        flags,
        meter_num: u16::from(t.meter.num),
        meter_den: u16::from(t.meter.den),
        tempo_bpm: t.tempo_bpm,
        position_beats: t.position_ticks / quarter,
        bar_start_beats: bar * bar_ticks / quarter,
        bar_number: bar as i32,
        reserved: 0,
        loop_start_beats: t.loop_range.start.0 as f64 / quarter,
        loop_end_beats: t.loop_range.end.0 as f64 / quarter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{Meter, Tick, TickRange};

    #[test]
    fn an_audio_bridge_keeps_the_callback_deadline_even_if_it_has_passed() {
        use komponist_ipc::{BridgeState, ShmRegion};

        let region = Arc::new(ShmRegion::create().unwrap());
        region.set_bridge_state(BridgeState::Active);
        let shared = Arc::new(Shared::new());
        shared.run.store(RUN_ACTIVE, Ordering::Release);
        let mut processor = ExternalProcessor::new(
            BridgeClient::new(region),
            shared.clone(),
            DeviceRole::Instrument,
            ProcessorSetup {
                sample_rate: 48_000.0,
                max_block: 32,
            },
        );
        let deadline = Instant::now();
        let context = ProcessContext {
            frames: 32,
            sample_rate: 48_000.0,
            transport: TransportInfo {
                playing: true,
                recording: false,
                looping: false,
                tempo_bpm: 120.0,
                meter: Meter::FOUR_FOUR,
                position_ticks: 0.0,
                loop_range: TickRange::new(Tick(0), Tick(3840)),
            },
            offline: false,
            deadline: Some(deadline),
        };
        let audio = [0.0; 32];
        assert!(assert_no_alloc::assert_no_alloc(|| processor.begin(
            &context,
            &[],
            [&audio; 2]
        )));
        let Outcome::Sent { deadline: wait, .. } = processor.begun.as_ref().unwrap().outcome else {
            panic!("expected the bridge request to be sent");
        };
        assert_eq!(wait, deadline);

        let mut left = [1.0; 32];
        let mut right = [1.0; 32];
        let mut io = StereoIo {
            input: [&audio; 2],
            output: [&mut left, &mut right],
        };
        let status = assert_no_alloc::assert_no_alloc(|| processor.process(&context, &[], &mut io));
        assert_eq!(status, ProcessStatus::Continue);
        assert_eq!(left, audio);
        assert_eq!(right, audio);
        assert_eq!(shared.late_blocks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn an_audio_bridge_delivers_held_parameter_values_after_activation() {
        use komponist_ipc::{BlockResult, BridgeServer, BridgeState, ShmRegion};

        let region = Arc::new(ShmRegion::create().unwrap());
        let shared = Arc::new(Shared::new());
        let mut server = BridgeServer::new(region.clone());
        let mut processor = ExternalProcessor::new(
            BridgeClient::new(region.clone()),
            shared.clone(),
            DeviceRole::Instrument,
            ProcessorSetup {
                sample_rate: 48_000.0,
                max_block: 32,
            },
        );
        let context = ProcessContext {
            frames: 32,
            sample_rate: 48_000.0,
            transport: TransportInfo {
                playing: true,
                recording: false,
                looping: false,
                tempo_bpm: 120.0,
                meter: Meter::FOUR_FOUR,
                position_ticks: 0.0,
                loop_range: TickRange::new(Tick(0), Tick(3840)),
            },
            offline: true,
            deadline: None,
        };
        let audio = [0.0; 32];
        assert_no_alloc::assert_no_alloc(|| {
            for value in [0.75, 0.2] {
                let events = [
                    Event {
                        offset: 4,
                        kind: EventKind::Param { id: 7, value },
                    },
                    Event {
                        offset: 5,
                        kind: EventKind::NoteOn {
                            key: 60,
                            velocity: 100,
                            channel: 0,
                            note_id: 1,
                        },
                    },
                ];
                assert!(matches!(
                    processor
                        .start(&context, &events, [&audio; 2], None)
                        .outcome,
                    Outcome::Fallback(_)
                ));
            }
            region.set_bridge_state(BridgeState::Active);
            shared.run.store(RUN_ACTIVE, Ordering::Release);
            assert!(matches!(
                processor.start(&context, &[], [&audio; 2], None).outcome,
                Outcome::Sent { .. }
            ));
            server.wait_request(Some(Instant::now())).unwrap();
            assert_eq!(
                server.pop_event(),
                Some(WireEvent::new(
                    0,
                    WireEventKind::ParamValue { id: 7, value: 0.2 }
                ))
            );
            assert_eq!(server.pop_event(), None);
            server.finish(BlockResult::default());
        });
    }

    #[test]
    fn midi_bridge_defers_parameters_to_keep_timed_note_releases_in_their_block() {
        use komponist_ipc::ShmRegion;

        let region = Arc::new(ShmRegion::create().unwrap());
        let mut processor = ExternalProcessor::new(
            BridgeClient::new(region),
            Arc::new(Shared::new()),
            DeviceRole::MidiEffect,
            ProcessorSetup {
                sample_rate: 48_000.0,
                max_block: 32,
            },
        );
        EventProcessor::set_param(&mut processor, 7, 0.25);
        let note_off = Event {
            offset: 31,
            kind: EventKind::NoteOff {
                key: 61,
                channel: 2,
                note_id: 91,
            },
        };
        let input = vec![note_off; INPUT_EVENT_CAPACITY + 1];
        assert_no_alloc::assert_no_alloc(|| {
            assert!(!processor.build_wire(&input));
            assert!(processor.wire.is_empty());
            assert!(processor.build_wire(&input[..INPUT_EVENT_CAPACITY]));
        });
        assert_eq!(processor.wire.len(), INPUT_EVENT_CAPACITY);
        assert_eq!(processor.wire.last(), Some(&to_wire(&note_off)));
        assert!(processor.build_wire(&[]));
        assert_eq!(
            processor.wire[0].decode(),
            Some(WireEventKind::ParamValue { id: 7, value: 0.25 }),
        );
    }

    #[test]
    fn midi_stage_discards_stale_and_partial_notes_and_resets_after_loss() {
        use std::sync::mpsc;

        use komponist_engine::processor::EventBuffer;
        use komponist_ipc::{BlockResult, BridgeServer, BridgeState, ShmRegion};

        let region = Arc::new(ShmRegion::create().unwrap());
        region.set_bridge_state(BridgeState::Active);
        let shared = Arc::new(Shared::new());
        shared.run.store(RUN_ACTIVE, Ordering::Release);
        let mut processor = ExternalProcessor::new(
            BridgeClient::new(region.clone()),
            shared.clone(),
            DeviceRole::MidiEffect,
            ProcessorSetup {
                sample_rate: 48_000.0,
                max_block: 32,
            },
        );
        let transport = TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick(0), Tick(3840)),
        };
        let context = ProcessContext {
            frames: 32,
            sample_rate: 48_000.0,
            transport,
            offline: true,
            deadline: None,
        };
        let note = WireEventKind::NoteOn {
            channel: 2,
            key: 64,
            velocity: 100,
            note_id: 31,
        };
        let expected_note = |offset| Event {
            offset,
            kind: EventKind::NoteOn {
                channel: 2,
                key: 64,
                velocity: 100,
                note_id: 31,
            },
        };
        let (timed_out, allow_late_reply) = mpsc::channel();
        let (published_late_reply, late_reply_ready) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let mut server = BridgeServer::new(region);
                for i in 0..5 {
                    let req = server
                        .wait_request(Some(Instant::now() + OFFLINE_DEADLINE))
                        .unwrap();
                    let push = |server: &mut BridgeServer, seq, offset, kind| {
                        assert!(server.push_output_event(WireEvent {
                            reserved: seq,
                            ..WireEvent::new(offset, kind)
                        }));
                    };
                    if i == 0 {
                        let sent = server.pop_event().unwrap();
                        assert_eq!(
                            sent.decode(),
                            Some(WireEventKind::ParamValue { id: 7, value: 0.25 })
                        );
                        push(&mut server, req.seq.wrapping_sub(1), 1, note);
                        push(
                            &mut server,
                            req.seq.wrapping_sub(1),
                            9,
                            WireEventKind::ParamGesture { id: 7, begin: true },
                        );
                        push(&mut server, req.seq, 3, note);
                        push(
                            &mut server,
                            req.seq,
                            4,
                            WireEventKind::ParamValue { id: 7, value: 0.5 },
                        );
                    } else if i == 1 {
                        push(&mut server, req.seq, 11, note);
                    } else if i == 2 || i == 4 {
                        assert_ne!(req.flags & BLOCK_FLAG_RESET, 0);
                        push(&mut server, req.seq, if i == 2 { 7 } else { 13 }, note);
                    } else {
                        // Publish only after the client has fallen back, regardless of scheduling.
                        allow_late_reply.recv_timeout(OFFLINE_DEADLINE).unwrap();
                        push(&mut server, req.seq, 11, note);
                    }
                    server.finish(BlockResult {
                        status: BlockStatus::Ok as u32,
                        dropped_events: u32::from(i == 1),
                        ..BlockResult::default()
                    });
                    if i == 3 {
                        published_late_reply.send(()).unwrap();
                    }
                }
            });

            let mut output = EventBuffer::new(16);
            let stage = processor.event_processor().unwrap();
            stage.set_param(7, 0.25);
            stage.process_events(&context, &[], &mut output.sink(context.frames));
            assert_eq!(output.events(), [expected_note(3)]);
            assert_eq!(
                processor.pop_parameter_edit(),
                Some(ParameterEdit {
                    offset: 0,
                    kind: ParameterEditKind::Gesture { id: 7, begin: true }
                }),
            );
            assert_eq!(
                processor.pop_parameter_edit(),
                Some(ParameterEdit {
                    offset: 4,
                    kind: ParameterEditKind::Value { id: 7, value: 0.5 }
                }),
            );

            processor.event_processor().unwrap().process_events(
                &context,
                &[],
                &mut output.sink(context.frames),
            );
            assert_eq!(
                output.events(),
                [Event {
                    offset: 0,
                    kind: EventKind::AllNotesOff
                }]
            );
            processor.event_processor().unwrap().process_events(
                &context,
                &[],
                &mut output.sink(context.frames),
            );
            assert_eq!(output.events(), [expected_note(7)]);

            let expired = ProcessContext {
                offline: false,
                deadline: Some(Instant::now()),
                ..context
            };
            processor.event_processor().unwrap().process_events(
                &expired,
                &[],
                &mut output.sink(expired.frames),
            );
            assert_eq!(shared.late_blocks.load(Ordering::Relaxed), 1);
            assert_eq!(
                output.events(),
                [Event {
                    offset: 0,
                    kind: EventKind::AllNotesOff
                }]
            );
            timed_out.send(()).unwrap();
            late_reply_ready.recv_timeout(OFFLINE_DEADLINE).unwrap();
            processor.event_processor().unwrap().process_events(
                &context,
                &[],
                &mut output.sink(context.frames),
            );
            assert_eq!(output.events(), [expected_note(13)]);
            assert!(processor.pop_parameter_edit().is_none());
        });
    }

    #[test]
    fn transport_in_quarter_notes() {
        let t = TransportInfo {
            playing: true,
            recording: false,
            looping: true,
            tempo_bpm: 124.0,
            meter: Meter { num: 3, den: 4 },
            // Bar 3 (index 2) of 3/4, beat 2.5.
            position_ticks: 2.0 * 2880.0 + 1.5 * 960.0,
            loop_range: TickRange::new(Tick(960), Tick(4 * 960)),
        };
        let w = wire_transport(&t);
        assert_eq!(w.flags, TRANSPORT_PLAYING | TRANSPORT_LOOPING);
        assert_eq!((w.meter_num, w.meter_den), (3, 4));
        assert_eq!(w.tempo_bpm, 124.0);
        assert_eq!(w.position_beats, 7.5);
        assert_eq!(w.bar_number, 2);
        assert_eq!(w.bar_start_beats, 6.0);
        assert_eq!((w.loop_start_beats, w.loop_end_beats), (1.0, 4.0));

        // Count-in: negative positions belong to bar −1.
        let w = wire_transport(&TransportInfo {
            position_ticks: -960.0,
            meter: Meter::FOUR_FOUR,
            ..t
        });
        assert_eq!(w.bar_number, -1);
        assert_eq!(w.bar_start_beats, -4.0);
        assert_eq!(w.position_beats, -1.0);
    }

    #[test]
    fn engine_events_on_the_wire() {
        let on = Event {
            offset: 5,
            kind: EventKind::NoteOn {
                key: 60,
                velocity: 100,
                channel: 2,
                note_id: 7,
            },
        };
        assert_eq!(
            to_wire(&on).decode(),
            Some(WireEventKind::NoteOn {
                channel: 2,
                key: 60,
                velocity: 100,
                note_id: 7,
            })
        );
        assert_eq!(to_wire(&on).offset, 5);
        let param = Event {
            offset: 0,
            kind: EventKind::Param { id: 3, value: 0.5 },
        };
        assert_eq!(
            to_wire(&param).decode(),
            Some(WireEventKind::ParamValue { id: 3, value: 0.5 })
        );
        let midi = Event {
            offset: 1,
            kind: EventKind::Midi([0xB0, 1, 64]),
        };
        assert_eq!(
            to_wire(&midi).decode(),
            Some(WireEventKind::Midi([0xB0, 1, 64]))
        );
    }
}
