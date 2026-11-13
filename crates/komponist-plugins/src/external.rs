//! The engine-side stand-in for a plugin running in a bridge process (ARCHITECTURE §8.1).
//!
//! Implements `komponist_engine::Processor` on top of `komponist_ipc::BridgeClient`. Per call:
//! convert the events to `WireEvent`s and the transport to `WireTransport`, `begin_block`, then
//! `wait_done` until the callback's shared deadline (`ProcessContext::deadline`: 60 % of the
//! callback period after the callback started, the same for every processor of the callback). Fallbacks (passthrough for effects, silence for instruments):
//! while the bridge loads (`Loading`), when a block is late or the bridge busy (counted as "late"),
//! and permanently once the bridge died (then `ProcessStatus::Failed`). Undelivered note-offs are
//! kept and sent with the next block that reaches the bridge.
//!
//! Created only by [`crate::PluginManager`]; lives on the audio thread; dropped on the UI thread
//! (engine garbage).
//!
//! Without a deadline in the context (calls outside the engine) the processor takes
//! [`DEADLINE_SHARE`] of its own call's period from its entry. Offline calls (export) wait up to
//! [`OFFLINE_DEADLINE`] instead, so a render never drops a block for timing reasons.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
    TransportInfo,
};
use komponist_ipc::shm::{
    BLOCK_FLAG_OFFLINE, BLOCK_FLAG_RESET, INPUT_EVENT_CAPACITY, MAX_BLOCK, TRANSPORT_LOOPING,
    TRANSPORT_PLAYING, TRANSPORT_RECORDING,
};
use komponist_ipc::{BlockError, BlockSpec, BridgeClient, WireEvent, WireEventKind, WireTransport};
use komponist_model::{DeviceRole, PPQ};

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
}

impl Shared {
    pub(crate) fn new() -> Shared {
        Shared {
            run: AtomicU8::new(RUN_LOADING),
            reactivating: AtomicBool::new(false),
            reactivate: Mutex::new(None),
            late_blocks: AtomicU64::new(0),
        }
    }
}

/// A [`Processor`] whose work happens in a bridge process.
#[derive(Debug)]
pub struct ExternalProcessor {
    client: BridgeClient,
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
            client,
            shared,
            role,
            setup,
            steady_time: 0,
            reset: false,
            wire: Vec::with_capacity(INPUT_EVENT_CAPACITY),
            pending: Vec::with_capacity(PENDING_CAPACITY),
            pending_all_off: false,
        }
    }

    /// RT. Writes the fallback for this call.
    fn fallback(&self, io: &mut StereoIo<'_>) {
        match self.role {
            DeviceRole::Instrument => io.silence(),
            DeviceRole::Effect => io.passthrough(),
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

    /// RT. Fills `wire` with the pending events (offset 0) followed by `events`; what does not fit
    /// is stashed.
    fn build_wire(&mut self, events: &[Event]) {
        self.wire.clear();
        if self.pending_all_off {
            self.wire
                .push(WireEvent::new(0, WireEventKind::AllNotesOff));
            self.pending_all_off = false;
        }
        self.wire.extend_from_slice(&self.pending);
        self.pending.clear();
        for event in events {
            let wire = to_wire(event);
            if self.wire.len() < INPUT_EVENT_CAPACITY {
                self.wire.push(wire);
            } else {
                self.stash(wire);
            }
        }
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
        self.shared.reactivating.store(true, Ordering::Release);
        *self
            .shared
            .reactivate
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(setup);
        Ok(())
    }

    /// RT: one shared-memory round trip with deadline, or the fallback.
    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let entered = Instant::now();
        let frames = io.frames();
        self.steady_time += frames as u64;
        match self.shared.run.load(Ordering::Acquire) {
            RUN_ACTIVE if !self.shared.reactivating.load(Ordering::Acquire) => {}
            RUN_DEAD => {
                self.fallback(io);
                return ProcessStatus::Failed;
            }
            _ => {
                self.forget_pending();
                self.fallback(io);
                return ProcessStatus::Continue;
            }
        }
        if frames == 0 || frames > MAX_BLOCK {
            self.fallback(io);
            return ProcessStatus::Continue;
        }

        self.build_wire(events);
        let mut flags = 0;
        if self.reset {
            flags |= BLOCK_FLAG_RESET;
        }
        if ctx.offline {
            flags |= BLOCK_FLAG_OFFLINE;
        }
        let spec = BlockSpec {
            frames: frames as u32,
            flags,
            steady_time: self.steady_time - frames as u64,
            transport: wire_transport(&ctx.transport),
        };
        let seq = match self.client.begin_block(&spec, io.input, &self.wire) {
            Ok(seq) => seq,
            Err(error) => {
                // Nothing reached the bridge: keep what must not get lost.
                self.stash_wire();
                if matches!(error, BlockError::Busy | BlockError::TooManyEvents) {
                    self.shared.late_blocks.fetch_add(1, Ordering::Relaxed);
                }
                self.fallback(io);
                return ProcessStatus::Continue;
            }
        };
        self.reset = false;
        self.wire.clear();
        let deadline = match ctx.deadline {
            _ if ctx.offline => entered + OFFLINE_DEADLINE,
            Some(deadline) => deadline,
            None => {
                let period = frames as f64 / f64::from(ctx.sample_rate);
                entered + Duration::from_secs_f64(period * DEADLINE_SHARE)
            }
        };
        let [left, right] = &mut io.output;
        match self
            .client
            .wait_done(seq, deadline, [&mut **left, &mut **right])
        {
            // `PluginError` / `Inactive` results carry the bridge's own fallback in the output.
            Ok(_) => ProcessStatus::Continue,
            Err(_) => {
                self.shared.late_blocks.fetch_add(1, Ordering::Relaxed);
                self.fallback(io);
                ProcessStatus::Continue
            }
        }
    }

    /// RT: the latency the bridge published in shared memory.
    fn latency(&self) -> u32 {
        self.client.region().latency()
    }

    /// RT: sets `BLOCK_FLAG_RESET` on the next block.
    fn reset(&mut self) {
        self.reset = true;
        // The plugin silences every voice on reset; old note-offs are moot.
        self.forget_pending();
    }
}

/// RT. An engine event on the wire.
fn to_wire(event: &Event) -> WireEvent {
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
