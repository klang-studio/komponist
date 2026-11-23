//! Engine handoff between streams (ARCHITECTURE §7, R1).
//!
//! The stream callback closure owns an [`EngineSlot`]: the boxed [`AudioProcess`] plus the MIDI
//! receiver. The UI side keeps the matching [`SlotHandle`]. When cpal tears the stream down, the
//! closure and with it the slot are dropped on a non-RT thread; the slot's `Drop` sends the engine
//! and the receiver back through the handle. Detach (export) moves the engine out while the stream
//! keeps running; attach moves one in. Both moves go through single-slot lock-free queues, so the
//! callback never allocates, frees or blocks.

use std::marker::PhantomData;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::midi::MidiReceiver;
use crate::process::{AudioProcess, CallbackInfo, MidiIn, OutputBuffer};
use crate::system::StreamStats;

/// A callback counts as late (an xrun) when it starts more than twice the expected period after
/// the previous one, plus this slack against scheduling jitter.
const LATE_SLACK: Duration = Duration::from_millis(1);

/// A lock-free single-item mailbox that moves a `Box<T>` between threads without allocating.
struct Mailbox<T> {
    item: AtomicPtr<T>,
    _owns: PhantomData<Box<T>>,
}

// SAFETY: the mailbox only moves ownership of a `Box<T>` from one thread to another (like a
// channel), so sharing it needs `T: Send`, not `T: Sync`.
unsafe impl<T: Send> Send for Mailbox<T> {}
// SAFETY: see above; all access goes through atomic swaps of the pointer.
unsafe impl<T: Send> Sync for Mailbox<T> {}

impl<T> Mailbox<T> {
    fn new() -> Mailbox<T> {
        Mailbox {
            item: AtomicPtr::new(ptr::null_mut()),
            _owns: PhantomData,
        }
    }

    /// RT. Puts `item` in if the mailbox is empty, else hands it back.
    fn put(&self, item: Box<T>) -> Result<(), Box<T>> {
        let raw = Box::into_raw(item);
        match self
            .item
            .compare_exchange(ptr::null_mut(), raw, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => Ok(()),
            // SAFETY: `raw` comes from `Box::into_raw` above and was not published.
            Err(_) => Err(unsafe { Box::from_raw(raw) }),
        }
    }

    /// RT. Takes the item out, if any.
    fn take(&self) -> Option<Box<T>> {
        let raw = self.item.swap(ptr::null_mut(), Ordering::SeqCst);
        // SAFETY: only `put` stores non-null pointers, each from `Box::into_raw`, and the swap
        // hands each of them out exactly once.
        (!raw.is_null()).then(|| unsafe { Box::from_raw(raw) })
    }
}

impl<T> Drop for Mailbox<T> {
    fn drop(&mut self) {
        drop(self.take());
    }
}

/// What the slot's drop hands back.
struct Returned<P> {
    engines: Vec<Box<P>>,
    midi: Option<MidiReceiver>,
}

/// State shared by an [`EngineSlot`] and its [`SlotHandle`].
struct Shared<P> {
    /// UI → callback: an engine to take over.
    attach: Mailbox<P>,
    /// Callback → UI: the engine handed out by a detach.
    detached: Mailbox<P>,
    /// UI → callback: the MIDI receiver of a slot created without one.
    midi: Mailbox<MidiReceiver>,
    /// Set by the UI, cleared by the callback once the engine is out.
    detach_requested: AtomicBool,
    /// The slot holds an engine or one is on its way in. Set by `attach`, cleared by a detach.
    occupied: AtomicBool,
    /// Set first in the slot's drop: a concurrent `attach` takes its engine back.
    closing: AtomicBool,
    /// Set last in the slot's drop, once `returned` is filled.
    closed: AtomicBool,
    /// Filled by the slot's drop (non-RT).
    returned: Mutex<Returned<P>>,
}

/// The callback's side: owns the engine while the stream runs.
pub struct EngineSlot<P: AudioProcess> {
    shared: Arc<Shared<P>>,
    engine: Option<Box<P>>,
    /// Boxed so a receiver attached through the mailbox is never freed on the audio thread.
    midi: Option<Box<MidiReceiver>>,
    stats: Arc<StreamStats>,
    /// `now` and period of the previous call, for late-callback detection.
    last: Option<(Instant, Duration)>,
    /// Xrun count after the previous call: a backend-reported xrun in between is not counted
    /// twice.
    seen_xruns: u32,
}

/// Result of [`SlotHandle::detach_step`].
pub(crate) enum DetachStep<P> {
    /// The engine is out.
    Engine(Box<P>),
    /// The slot holds an engine; a detach was requested, the callback has not served it yet.
    Pending,
    /// The slot holds no engine and none is on its way out.
    Empty,
}

/// The UI's side of an [`EngineSlot`].
pub struct SlotHandle<P: AudioProcess> {
    shared: Arc<Shared<P>>,
}

impl<P: AudioProcess> EngineSlot<P> {
    /// Non-RT. A slot holding `engine` (already prepared for the stream) and `midi`, recording
    /// callback load and xruns into `stats`.
    pub fn new(
        engine: Box<P>,
        midi: MidiReceiver,
        stats: Arc<StreamStats>,
    ) -> (EngineSlot<P>, SlotHandle<P>) {
        let (mut slot, handle) = EngineSlot::empty(stats);
        slot.engine = Some(engine);
        let mut midi = Box::new(midi);
        midi.mark_attached();
        slot.midi = Some(midi);
        slot.shared.occupied.store(true, Ordering::SeqCst);
        (slot, handle)
    }

    /// Non-RT. A slot without engine and MIDI receiver: it writes silence until
    /// [`SlotHandle::attach`] and [`SlotHandle::attach_midi`] hand them in.
    pub(crate) fn empty(stats: Arc<StreamStats>) -> (EngineSlot<P>, SlotHandle<P>) {
        let shared = Arc::new(Shared {
            attach: Mailbox::new(),
            detached: Mailbox::new(),
            midi: Mailbox::new(),
            detach_requested: AtomicBool::new(false),
            occupied: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            returned: Mutex::new(Returned {
                engines: Vec::new(),
                midi: None,
            }),
        });
        let slot = EngineSlot {
            shared: Arc::clone(&shared),
            engine: None,
            midi: None,
            stats,
            last: None,
            seen_xruns: 0,
        };
        (slot, SlotHandle { shared })
    }

    /// RT, called by the backend callback. Picks up an attached engine and serves a detach request
    /// first; then runs [`AudioProcess::process`] (with FTZ/DAZ set and the load measured) or, with
    /// no engine, writes silence.
    pub fn process(&mut self, out: &mut OutputBuffer<'_>, info: &CallbackInfo) {
        self.process_input(out, info, &crate::InputBuffer::empty());
    }

    /// RT. The duplex form of [`EngineSlot::process`], with borrowed captured audio.
    pub fn process_input(
        &mut self,
        out: &mut OutputBuffer<'_>,
        info: &CallbackInfo,
        input: &crate::InputBuffer<'_>,
    ) {
        let start = Instant::now();
        self.exchange();
        if let Some(receiver) = self.midi.as_deref_mut() {
            receiver.begin(info, &self.stats);
        }

        if let Some(engine) = self.engine.as_deref_mut() {
            crate::enable_flush_to_zero();
            let mut midi = match self.midi.as_deref_mut() {
                Some(receiver) => MidiIn::new(receiver, *info),
                None => MidiIn::empty(*info),
            };
            engine.process_input(out, info, &mut midi, input);
        } else {
            out.silence();
            if let Some(receiver) = self.midi.as_deref_mut() {
                receiver.discard_before(info.now, &self.stats);
            }
        }

        let period = crate::frames_to_duration(u64::from(info.frames), info.sample_rate);
        let load = if period.is_zero() {
            0.0
        } else {
            start.elapsed().as_secs_f32() / period.as_secs_f32()
        };
        self.stats.record_callback(load);
        self.detect_late(info.now, period);
    }

    /// RT. Takes over an attached engine and MIDI receiver, then serves a detach request.
    fn exchange(&mut self) {
        let shared = &*self.shared;
        if self.engine.is_none() {
            self.engine = shared.attach.take();
        }
        if self.midi.is_none() {
            self.midi = shared.midi.take();
            if let Some(midi) = self.midi.as_deref_mut() {
                midi.mark_attached();
            }
        }
        if !shared.detach_requested.load(Ordering::SeqCst) {
            return;
        }
        // An engine attached right before the request is already in the mailbox (SeqCst).
        if self.engine.is_none() {
            self.engine = shared.attach.take();
        }
        match self.engine.take() {
            Some(engine) => match shared.detached.put(engine) {
                Ok(()) => {
                    shared.occupied.store(false, Ordering::SeqCst);
                    shared.detach_requested.store(false, Ordering::SeqCst);
                }
                // The UI has not collected an earlier detached engine yet: keep this one (it must
                // not be dropped here) and try again next time.
                Err(engine) => self.engine = Some(engine),
            },
            None => shared.detach_requested.store(false, Ordering::SeqCst),
        }
    }

    /// RT. Counts a late callback as an xrun unless the backend reported one meanwhile.
    fn detect_late(&mut self, now: Instant, period: Duration) {
        if let Some((last_now, last_period)) = self.last {
            let expected = period.max(last_period);
            let late = now.saturating_duration_since(last_now) > expected * 2 + LATE_SLACK;
            if late && self.stats.xruns() == self.seen_xruns {
                self.stats.record_xrun();
            }
        }
        self.last = Some((now, period));
        self.seen_xruns = self.stats.xruns();
    }
}

impl<P: AudioProcess> Drop for EngineSlot<P> {
    /// Non-RT (stream teardown): hands the engine and the MIDI receiver back to the handle.
    fn drop(&mut self) {
        let shared = &*self.shared;
        shared.closing.store(true, Ordering::SeqCst);
        {
            let mut returned = shared
                .returned
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            returned.engines.extend(self.engine.take());
            returned.engines.extend(shared.attach.take());
            let midi = self.midi.take().or_else(|| shared.midi.take());
            if let Some(midi) = midi {
                returned.midi = Some(*midi);
            }
        }
        shared.closed.store(true, Ordering::SeqCst);
    }
}

impl<P: AudioProcess> SlotHandle<P> {
    /// Asks the callback to hand the engine out at its next run.
    pub fn request_detach(&self) {
        self.shared.detach_requested.store(true, Ordering::SeqCst);
    }

    /// The engine, once handed out by a detach or by the slot's drop at stream teardown.
    pub fn try_take_engine(&mut self) -> Option<Box<P>> {
        self.shared.detached.take().or_else(|| {
            let mut returned = self.returned();
            returned.engines.pop()
        })
    }

    /// One step of a detach: the engine if it is out, else (while the slot still holds one or
    /// one is on its way in) a detach request and `DetachStep::Pending`, else `DetachStep::Empty`.
    pub(crate) fn detach_step(&mut self) -> DetachStep<P> {
        self.detach_step_with(|| {})
    }

    /// [`SlotHandle::detach_step`] with `between` run after the occupancy was read and before the
    /// engine is looked for (tests use it to run the callback exactly there).
    ///
    /// The order matters: the callback puts the engine into the `detached` mailbox *before* it
    /// clears `occupied`. Reading `occupied` first means "not occupied" implies the engine is
    /// already in the mailbox when it is looked for. The reverse order could see an empty mailbox,
    /// then the callback finishing, then "not occupied": a hand-out nobody notices.
    pub(crate) fn detach_step_with(&mut self, between: impl FnOnce()) -> DetachStep<P> {
        let occupied = self.is_occupied();
        between();
        if let Some(engine) = self.try_take_engine() {
            DetachStep::Engine(engine)
        } else if occupied {
            self.request_detach();
            DetachStep::Pending
        } else {
            DetachStep::Empty
        }
    }

    /// The MIDI receiver, once the slot was dropped at stream teardown.
    pub fn try_take_midi(&mut self) -> Option<MidiReceiver> {
        self.returned().midi.take()
    }

    /// Hands a prepared engine to the running slot; returns it if the slot already holds one or is
    /// gone.
    ///
    /// If the slot is dropped while the engine is on its way in, the engine either comes back
    /// here as `Err` or through [`SlotHandle::try_take_engine`]; it is never lost.
    pub fn attach(&mut self, engine: Box<P>) -> Result<(), Box<P>> {
        let shared = &*self.shared;
        if shared.closing.load(Ordering::SeqCst) {
            return Err(engine);
        }
        if shared
            .occupied
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(engine);
        }
        if let Err(engine) = shared.attach.put(engine) {
            shared.occupied.store(false, Ordering::SeqCst);
            return Err(engine);
        }
        // The slot's drop sets `closing` before it empties the mailbox: if it is set now, either
        // the drop took the engine (it comes back through `try_take_engine`) or it is still here.
        if shared.closing.load(Ordering::SeqCst)
            && let Some(engine) = shared.attach.take()
        {
            return Err(engine);
        }
        Ok(())
    }

    /// Hands the MIDI receiver to a slot created without one; returns it if the slot already has
    /// one or is gone.
    pub(crate) fn attach_midi(&mut self, midi: MidiReceiver) -> Result<(), MidiReceiver> {
        let shared = &*self.shared;
        if shared.closing.load(Ordering::SeqCst) {
            return Err(midi);
        }
        if let Err(midi) = shared.midi.put(Box::new(midi)) {
            return Err(*midi);
        }
        if shared.closing.load(Ordering::SeqCst)
            && let Some(midi) = shared.midi.take()
        {
            return Err(*midi);
        }
        Ok(())
    }

    /// Whether the slot holds an engine or one is on its way in.
    pub(crate) fn is_occupied(&self) -> bool {
        self.shared.occupied.load(Ordering::SeqCst)
    }

    /// Whether the slot was dropped (the stream is gone).
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    fn returned(&self) -> std::sync::MutexGuard<'_, Returned<P>> {
        self.shared
            .returned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl<P: AudioProcess> core::fmt::Debug for EngineSlot<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EngineSlot").finish_non_exhaustive()
    }
}

impl<P: AudioProcess> core::fmt::Debug for SlotHandle<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SlotHandle").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::{MidiPortId, queue};
    use crate::testing::TestEngine;
    use assert_no_alloc::assert_no_alloc;

    fn info(now: Instant, frames: u32) -> CallbackInfo {
        CallbackInfo {
            sample_rate: 48_000,
            frames,
            now,
            output_latency: Duration::ZERO,
        }
    }

    /// One callback of 64 stereo frames at `now`, inside `assert_no_alloc`; returns the output.
    fn run_at(slot: &mut EngineSlot<TestEngine>, now: Instant) -> [f32; 128] {
        let mut data = [7.0f32; 128];
        assert_no_alloc(|| slot.process(&mut OutputBuffer::new(&mut data, 2), &info(now, 64)));
        data
    }

    fn run(slot: &mut EngineSlot<TestEngine>) -> [f32; 128] {
        run_at(slot, Instant::now())
    }

    fn address(engine: &TestEngine) -> usize {
        engine as *const TestEngine as usize
    }

    /// The race behind a flaky `Detached`: the callback serves the detach request exactly between
    /// the UI's occupancy check and its look into the hand-out mailbox. The step must still find
    /// the engine (it used to report "nothing to detach").
    #[test]
    fn detach_step_finds_an_engine_handed_out_between_its_checks() {
        let (engine, _probe) = TestEngine::boxed(0.5);
        let (_sender, receiver) = queue(4);
        let (mut slot, mut handle) =
            EngineSlot::new(engine, receiver, Arc::new(StreamStats::new()));
        run(&mut slot);
        assert!(matches!(handle.detach_step(), DetachStep::Pending));
        let step = handle.detach_step_with(|| {
            run(&mut slot);
        });
        assert!(matches!(step, DetachStep::Engine(_)));
        assert!(!handle.is_occupied());
        assert!(matches!(handle.detach_step(), DetachStep::Empty));
    }

    #[test]
    fn new_slot_runs_the_engine_and_records_stats() {
        let (engine, probe) = TestEngine::boxed(0.25);
        let (_sender, receiver) = queue(4);
        let stats = Arc::new(StreamStats::new());
        let (mut slot, handle) = EngineSlot::new(engine, receiver, Arc::clone(&stats));
        assert!(handle.is_occupied());
        for _ in 0..3 {
            assert!(run(&mut slot).iter().all(|&s| s == 0.25));
        }
        assert_eq!(probe.calls(), 3);
        assert_eq!(stats.callbacks(), 3);
        assert!(stats.cpu_load() >= 0.0);
        assert_eq!(stats.xruns(), 0);
    }

    #[test]
    fn detach_hands_out_the_same_engine_and_then_silence() {
        let (engine, probe) = TestEngine::boxed(0.5);
        let original = address(&engine);
        let (_sender, receiver) = queue(4);
        let (mut slot, mut handle) =
            EngineSlot::new(engine, receiver, Arc::new(StreamStats::new()));
        run(&mut slot);
        handle.request_detach();
        assert!(
            handle.try_take_engine().is_none(),
            "not before the callback ran"
        );
        // The request is served before `process`: this callback is already silent.
        assert!(run(&mut slot).iter().all(|&s| s == 0.0));
        let engine = handle.try_take_engine().expect("handed out");
        assert_eq!(address(&engine), original);
        assert_eq!(engine.processed, 1);
        assert!(!handle.is_occupied());
        assert!(run(&mut slot).iter().all(|&s| s == 0.0));
        assert_eq!(probe.calls(), 1);
        // A request without an engine is dropped, so a later attach is not detached at once.
        handle.request_detach();
        run(&mut slot);
        handle.attach(engine).unwrap();
        assert!(run(&mut slot).iter().all(|&s| s == 0.5));
        assert_eq!(probe.calls(), 2);
    }

    #[test]
    fn attach_takes_over_at_the_next_callback() {
        let (mut slot, mut handle) = EngineSlot::empty(Arc::new(StreamStats::new()));
        assert!(run(&mut slot).iter().all(|&s| s == 0.0));
        let (engine, probe) = TestEngine::boxed(1.0);
        handle.attach(engine).unwrap();
        assert!(handle.is_occupied());
        let (second, _) = TestEngine::boxed(2.0);
        let second = handle
            .attach(second)
            .expect_err("slot already has an engine");
        assert_eq!(second.value, 2.0);
        assert!(run(&mut slot).iter().all(|&s| s == 1.0));
        assert_eq!(probe.calls(), 1);
    }

    #[test]
    fn drop_returns_engine_and_midi() {
        let (engine, _) = TestEngine::boxed(0.0);
        let original = address(&engine);
        let (sender, receiver) = queue(4);
        let (mut slot, mut handle) =
            EngineSlot::new(engine, receiver, Arc::new(StreamStats::new()));
        run(&mut slot);
        assert!(!handle.is_closed());
        assert!(handle.try_take_midi().is_none());
        drop(slot);
        assert!(handle.is_closed());
        let engine = handle.try_take_engine().expect("returned by drop");
        assert_eq!(address(&engine), original);
        let mut midi = handle.try_take_midi().expect("returned by drop");
        // It is the same queue.
        assert!(sender.push(MidiPortId(0), [0x90, 1, 1]));
        assert!(midi.pop_before(Instant::now()).is_some());
        let engine = handle.attach(engine).expect_err("slot is gone");
        assert_eq!(address(&engine), original);
    }

    #[test]
    fn pending_attach_comes_back_when_the_slot_drops() {
        let (slot, mut handle) = EngineSlot::<TestEngine>::empty(Arc::new(StreamStats::new()));
        let (engine, _) = TestEngine::boxed(0.0);
        let original = address(&engine);
        handle.attach(engine).unwrap();
        let (_sender, receiver) = queue(4);
        handle.attach_midi(receiver).unwrap();
        drop(slot);
        assert_eq!(address(&handle.try_take_engine().unwrap()), original);
        assert!(handle.try_take_midi().is_some());
        assert!(handle.try_take_engine().is_none());
    }

    #[test]
    fn detach_waits_until_the_previous_engine_was_collected() {
        let (first, _) = TestEngine::boxed(1.0);
        let first_address = address(&first);
        let (_sender, receiver) = queue(4);
        let (mut slot, mut handle) = EngineSlot::new(first, receiver, Arc::new(StreamStats::new()));
        handle.request_detach();
        run(&mut slot);
        // `first` waits in the mailbox; a second engine goes in and is asked out again.
        let (second, _) = TestEngine::boxed(2.0);
        let second_address = address(&second);
        handle.attach(second).unwrap();
        handle.request_detach();
        // The mailbox is full: the slot keeps `second` (it must not drop it) and runs it.
        assert!(run(&mut slot).iter().all(|&s| s == 2.0));
        assert_eq!(address(&handle.try_take_engine().unwrap()), first_address);
        assert!(run(&mut slot).iter().all(|&s| s == 0.0));
        assert_eq!(address(&handle.try_take_engine().unwrap()), second_address);
    }

    #[test]
    fn midi_reaches_the_engine_and_is_discarded_without_one() {
        let (sender, receiver) = queue(8);
        let (mut slot, mut handle) = EngineSlot::empty(Arc::new(StreamStats::new()));
        handle.attach_midi(receiver).unwrap();
        // No engine: stale events are thrown away instead of reaching a later engine.
        assert!(sender.push(MidiPortId(3), [0x90, 64, 1]));
        run_at(&mut slot, Instant::now() + Duration::from_millis(1));
        let (engine, probe) = TestEngine::boxed(0.0);
        handle.attach(engine).unwrap();
        run(&mut slot);
        assert_eq!(probe.events.load(Ordering::SeqCst), 0);

        assert!(sender.push(MidiPortId(3), [0x90, 60, 100]));
        run_at(&mut slot, Instant::now() + Duration::from_micros(100));
        assert_eq!(probe.events.load(Ordering::SeqCst), 1);
        assert_eq!(
            probe.last_event_data.load(Ordering::SeqCst),
            0x0090_3C64,
            "note on 60, velocity 100"
        );
        // 100 µs before the end of a 64-frame (1.33 ms) period: near the last frame.
        let frame = probe.last_event_frame.load(Ordering::SeqCst);
        assert!((50..64).contains(&frame), "frame {frame}");
    }

    /// A device switch: the old stream returns the receiver, the new one takes it over up to
    /// seconds later (a cpal build may take 3 s). What queued up meanwhile does not fire as a
    /// burst, and the note-off lost with it does not leave the note hanging.
    #[test]
    fn a_new_stream_discards_stale_midi_and_releases_held_notes() {
        let stats = Arc::new(StreamStats::new());
        let (engine, probe) = TestEngine::boxed(0.0);
        let (sender, receiver) = queue(8);
        let (mut slot, mut handle) = EngineSlot::new(engine, receiver, Arc::clone(&stats));
        assert!(sender.push(MidiPortId(2), [0x90, 60, 100]));
        run_at(&mut slot, Instant::now() + Duration::from_micros(100));
        assert_eq!(probe.events.load(Ordering::SeqCst), 1);
        drop(slot);
        let engine = handle.try_take_engine().unwrap();
        let midi = handle.try_take_midi().unwrap();

        assert!(sender.push(MidiPortId(2), [0x80, 60, 0]));
        assert!(sender.push(MidiPortId(2), [0xb0, 1, 64]));
        let (mut slot, mut handle) = EngineSlot::empty(Arc::clone(&stats));
        handle.attach(engine).unwrap();
        handle.attach_midi(midi).unwrap();
        // One second later: both messages are stale.
        run_at(&mut slot, Instant::now() + Duration::from_secs(1));
        assert_eq!(stats.midi_stale(), 2);
        assert_eq!(probe.events.load(Ordering::SeqCst), 2);
        assert_eq!(
            probe.last_event_data.load(Ordering::SeqCst),
            0x0080_3C00,
            "the release of note 60"
        );
        assert_eq!(probe.last_event_frame.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn late_callbacks_count_as_xruns_once() {
        let stats = Arc::new(StreamStats::new());
        let (mut slot, _handle) = EngineSlot::<TestEngine>::empty(Arc::clone(&stats));
        let period = crate::frames_to_duration(64, 48_000);
        let t0 = Instant::now();
        run_at(&mut slot, t0);
        run_at(&mut slot, t0 + period);
        run_at(&mut slot, t0 + period * 2);
        assert_eq!(stats.xruns(), 0);
        // Three periods plus the slack late.
        let late = t0 + period * 5 + LATE_SLACK * 2;
        run_at(&mut slot, late);
        assert_eq!(stats.xruns(), 1);
        // The backend reported this one already: not counted again.
        stats.record_xrun();
        run_at(&mut slot, late + period * 5);
        assert_eq!(stats.xruns(), 2);
        assert_eq!(stats.callbacks(), 5);
    }

    #[test]
    fn attach_racing_with_teardown_never_loses_the_engine() {
        for _ in 0..500 {
            let (slot, mut handle) = EngineSlot::<TestEngine>::empty(Arc::new(StreamStats::new()));
            let (engine, _) = TestEngine::boxed(0.0);
            let original = address(&engine);
            let teardown = std::thread::spawn(move || drop(slot));
            let back = match handle.attach(engine) {
                Ok(()) => {
                    teardown.join().unwrap();
                    handle.try_take_engine()
                }
                Err(engine) => {
                    teardown.join().unwrap();
                    Some(engine)
                }
            };
            assert_eq!(back.map(|e| address(&e)), Some(original));
            assert!(handle.try_take_engine().is_none());
        }
    }
}
