//! Test engine shared by the unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::process::{AudioProcess, CallbackInfo, MidiIn, OutputBuffer, StreamParams};

/// Capacity of [`TestEngine::infos`].
pub(crate) const RECORDED_CALLS: usize = 8;

/// What a [`TestEngine`] reports to the test thread.
#[derive(Debug, Default)]
pub(crate) struct Probe {
    pub(crate) prepares: AtomicU32,
    pub(crate) sample_rate: AtomicU32,
    pub(crate) max_frames: AtomicU32,
    pub(crate) channels: AtomicU32,
    /// Address of the engine at its last `prepare`.
    pub(crate) address: AtomicUsize,
    pub(crate) calls: AtomicU64,
    /// Largest `out.frames()` seen.
    pub(crate) max_seen_frames: AtomicU32,
    pub(crate) events: AtomicU32,
    /// Frame offset of the last MIDI event.
    pub(crate) last_event_frame: AtomicU32,
    /// Status and data bytes of the last MIDI event, packed big-endian into the low 24 bits.
    pub(crate) last_event_data: AtomicU32,
}

impl Probe {
    pub(crate) fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    pub(crate) fn params(&self) -> StreamParams {
        StreamParams {
            sample_rate: self.sample_rate.load(Ordering::SeqCst),
            max_frames: self.max_frames.load(Ordering::SeqCst),
            channels: self.channels.load(Ordering::SeqCst) as u16,
        }
    }
}

/// Fills its output with `value` and records what it sees, without allocating in `process`.
#[derive(Debug)]
pub(crate) struct TestEngine {
    pub(crate) probe: Arc<Probe>,
    pub(crate) value: f32,
    /// Internal state that must survive stream rebuilds.
    pub(crate) processed: u64,
    /// The first [`RECORDED_CALLS`] callback infos.
    pub(crate) infos: [Option<CallbackInfo>; RECORDED_CALLS],
}

impl TestEngine {
    pub(crate) fn boxed(value: f32) -> (Box<TestEngine>, Arc<Probe>) {
        let probe = Arc::new(Probe::default());
        let engine = Box::new(TestEngine {
            probe: Arc::clone(&probe),
            value,
            processed: 0,
            infos: [None; RECORDED_CALLS],
        });
        (engine, probe)
    }
}

impl AudioProcess for TestEngine {
    fn prepare(&mut self, params: &StreamParams) {
        let probe = &self.probe;
        probe
            .sample_rate
            .store(params.sample_rate, Ordering::SeqCst);
        probe.max_frames.store(params.max_frames, Ordering::SeqCst);
        probe
            .channels
            .store(u32::from(params.channels), Ordering::SeqCst);
        probe
            .address
            .store(self as *const TestEngine as usize, Ordering::SeqCst);
        probe.prepares.fetch_add(1, Ordering::SeqCst);
    }

    fn process(&mut self, out: &mut OutputBuffer<'_>, info: &CallbackInfo, midi: &mut MidiIn<'_>) {
        while let Some(event) = midi.pop() {
            let [status, data1, data2] = event.data;
            let packed = u32::from_be_bytes([0, status, data1, data2]);
            self.probe
                .last_event_frame
                .store(midi.frame_of(event.time), Ordering::SeqCst);
            self.probe.last_event_data.store(packed, Ordering::SeqCst);
            self.probe.events.fetch_add(1, Ordering::SeqCst);
        }
        out.data_mut().fill(self.value);
        if let Some(slot) = self.infos.get_mut(self.processed as usize) {
            *slot = Some(*info);
        }
        self.processed += 1;
        self.probe
            .max_seen_frames
            .fetch_max(out.frames() as u32, Ordering::SeqCst);
        self.probe.calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// Polls `condition` every millisecond until it holds or `timeout` passes.
pub(crate) fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
