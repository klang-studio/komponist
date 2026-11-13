//! Processors for tests of the engine and of crates that drive it (ARCHITECTURE §6.2).
//!
//! - [`TestSine`]: a polyphonic instrument. Each note is a cosine at the key's pitch with
//!   amplitude `TEST_SINE_LEVEL × velocity / 127`, starting at its peak and stopping dead at its
//!   note-off, so note starts and ends are visible sample-exactly in the output. Optionally logs
//!   every call, event and reset with absolute frame numbers ([`TestSine::logged`]).
//! - [`TestGain`]: an effect multiplying by a gain (parameter [`TEST_GAIN_PARAM`]), optionally
//!   delaying by a fixed latency (for delay compensation tests).
//!
//! Both are RT-safe: `process` and `reset` never allocate (the log is a preallocated rtrb queue).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::processor::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, ProcessStatus, Processor, ProcessorError,
    StereoIo, TransportInfo,
};

/// Peak amplitude of a [`TestSine`] note at velocity 127.
pub const TEST_SINE_LEVEL: f32 = 0.5;

/// Voices of a [`TestSine`]; a further note steals the oldest voice.
pub const TEST_SINE_VOICES: usize = 32;

/// Parameter id of [`TestGain`]'s gain (linear).
pub const TEST_GAIN_PARAM: u32 = 0;

/// What a logged [`TestSine`] saw. Frames count from the processor's first call (the sum of all
/// call lengths before), so they equal engine output frames while the processor runs every call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Logged {
    /// A `process` call starting at `frame`.
    Call {
        /// First frame of the call.
        frame: u64,
        /// Frames in the call.
        frames: u32,
        /// The transport passed in.
        transport: TransportInfo,
        /// Offline render.
        offline: bool,
    },
    /// An event at `frame` (call start + offset).
    Event {
        /// Absolute frame.
        frame: u64,
        /// The event.
        kind: EventKind,
    },
    /// `reset` was called after `frame` frames.
    Reset {
        /// Frames processed before.
        frame: u64,
    },
}

/// The reading end of a [`TestSine`] log.
#[derive(Debug)]
pub struct EventLog {
    entries: rtrb::Consumer<Logged>,
    lost: Arc<AtomicU32>,
}

impl EventLog {
    /// Everything logged since the last call, in order.
    pub fn drain(&mut self) -> Vec<Logged> {
        let mut out = Vec::new();
        while let Ok(entry) = self.entries.pop() {
            out.push(entry);
        }
        out
    }

    /// Entries lost because the log was full.
    pub fn lost(&self) -> u32 {
        self.lost.load(Ordering::Relaxed)
    }
}

struct Log {
    entries: rtrb::Producer<Logged>,
    lost: Arc<AtomicU32>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    active: bool,
    key: u8,
    channel: u8,
    note_id: u32,
    amp: f64,
    /// Radians per sample.
    step: f64,
    /// Samples since the note-on.
    t: u64,
    /// Note-on order, for stealing.
    age: u64,
}

/// A polyphonic cosine instrument for tests (see the module docs).
pub struct TestSine {
    voices: [Voice; TEST_SINE_VOICES],
    sample_rate: f64,
    frames: u64,
    notes: u64,
    log: Option<Log>,
}

impl TestSine {
    /// A test instrument without log.
    pub fn new() -> TestSine {
        TestSine {
            voices: [Voice::default(); TEST_SINE_VOICES],
            sample_rate: 48_000.0,
            frames: 0,
            notes: 0,
            log: None,
        }
    }

    /// A test instrument logging into a queue of `capacity` entries, and the log's reading end.
    pub fn logged(capacity: usize) -> (TestSine, EventLog) {
        let (entries, reader) = rtrb::RingBuffer::new(capacity);
        let lost = Arc::new(AtomicU32::new(0));
        let mut sine = TestSine::new();
        sine.log = Some(Log {
            entries,
            lost: lost.clone(),
        });
        (
            sine,
            EventLog {
                entries: reader,
                lost,
            },
        )
    }

    fn log(&mut self, entry: Logged) {
        if let Some(log) = &mut self.log
            && log.entries.push(entry).is_err()
        {
            log.lost.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn apply(&mut self, event: &Event) {
        match event.kind {
            EventKind::NoteOn {
                key,
                velocity,
                channel,
                note_id,
            } => {
                let index = self
                    .voices
                    .iter()
                    .position(|v| v.active && v.key == key && v.channel == channel)
                    .or_else(|| self.voices.iter().position(|v| !v.active))
                    .unwrap_or_else(|| {
                        let oldest = self.voices.iter().map(|v| v.age).min().unwrap_or(0);
                        self.voices
                            .iter()
                            .position(|v| v.age == oldest)
                            .unwrap_or(0)
                    });
                let hz = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
                self.notes += 1;
                self.voices[index] = Voice {
                    active: true,
                    key,
                    channel,
                    note_id,
                    amp: f64::from(TEST_SINE_LEVEL) * f64::from(velocity) / 127.0,
                    step: core::f64::consts::TAU * hz / self.sample_rate,
                    t: 0,
                    age: self.notes,
                };
            }
            EventKind::NoteOff {
                key,
                channel,
                note_id,
            } => {
                for v in &mut self.voices {
                    let same = if note_id != NOTE_ID_NONE && v.note_id != NOTE_ID_NONE {
                        v.note_id == note_id
                    } else {
                        v.key == key && v.channel == channel
                    };
                    if v.active && same {
                        v.active = false;
                    }
                }
            }
            EventKind::AllNotesOff => {
                for v in &mut self.voices {
                    v.active = false;
                }
            }
            EventKind::Midi(_) | EventKind::Param { .. } => {}
        }
    }
}

impl Default for TestSine {
    fn default() -> TestSine {
        TestSine::new()
    }
}

impl Processor for TestSine {
    fn activate(&mut self, sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        if !(sample_rate > 0.0 && sample_rate.is_finite()) {
            return Err(ProcessorError::UnsupportedSampleRate(sample_rate));
        }
        self.sample_rate = f64::from(sample_rate);
        Ok(())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let n = io.frames();
        self.log(Logged::Call {
            frame: self.frames,
            frames: ctx.frames,
            transport: ctx.transport,
            offline: ctx.offline,
        });
        let mut next = 0;
        for i in 0..n {
            while let Some(event) = events.get(next).filter(|e| e.offset as usize <= i) {
                self.apply(event);
                let frame = self.frames + i as u64;
                self.log(Logged::Event {
                    frame,
                    kind: event.kind,
                });
                next += 1;
            }
            let mut sum = 0.0;
            for v in self.voices.iter_mut().filter(|v| v.active) {
                sum += v.amp * (v.step * v.t as f64).cos();
                v.t += 1;
            }
            io.output[0][i] = sum as f32;
            io.output[1][i] = sum as f32;
        }
        self.frames += n as u64;
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.active = false;
        }
        let frame = self.frames;
        self.log(Logged::Reset { frame });
    }
}

/// A gain effect for tests, with an optional fixed latency (see the module docs).
pub struct TestGain {
    gain: f32,
    latency: u32,
    line: Box<[f32]>,
    pos: usize,
}

impl TestGain {
    /// Multiplies by `gain`, no latency.
    pub fn new(gain: f32) -> TestGain {
        TestGain::with_latency(gain, 0)
    }

    /// Multiplies by `gain` and delays by `latency` frames (reported by `latency()`).
    pub fn with_latency(gain: f32, latency: u32) -> TestGain {
        TestGain {
            gain,
            latency,
            line: vec![0.0; 2 * latency as usize].into_boxed_slice(),
            pos: 0,
        }
    }
}

impl Processor for TestGain {
    fn activate(&mut self, _sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let n = io.frames();
        let latency = self.latency as usize;
        let mut next = 0;
        for i in 0..n {
            while let Some(event) = events.get(next).filter(|e| e.offset as usize <= i) {
                if let EventKind::Param {
                    id: TEST_GAIN_PARAM,
                    value,
                } = event.kind
                {
                    self.gain = value as f32;
                }
                next += 1;
            }
            let (mut l, mut r) = (io.input[0][i], io.input[1][i]);
            if latency > 0 {
                let (line_l, line_r) = self.line.split_at_mut(latency);
                (l, line_l[self.pos]) = (line_l[self.pos], l);
                (r, line_r[self.pos]) = (line_r[self.pos], r);
                self.pos = (self.pos + 1) % latency;
            }
            io.output[0][i] = l * self.gain;
            io.output[1][i] = r * self.gain;
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        self.line.fill(0.0);
        self.pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{Meter, Tick, TickRange};

    fn ctx(frames: u32) -> ProcessContext {
        ProcessContext {
            frames,
            sample_rate: 48_000.0,
            transport: TransportInfo {
                playing: false,
                recording: false,
                looping: false,
                tempo_bpm: 120.0,
                meter: Meter::FOUR_FOUR,
                position_ticks: 0.0,
                loop_range: TickRange::new(Tick::ZERO, Tick::ZERO),
            },
            offline: false,
        }
    }

    fn run(p: &mut dyn Processor, input: &[f32], events: &[Event]) -> Vec<f32> {
        let n = input.len();
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        let mut io = StereoIo {
            input: [input, input],
            output: [&mut l, &mut r],
        };
        p.process(&ctx(n as u32), events, &mut io);
        assert_eq!(l, r);
        l
    }

    #[test]
    fn sine_starts_at_its_peak_and_stops_dead() {
        let (mut sine, mut log) = TestSine::logged(16);
        let on = Event {
            offset: 3,
            kind: EventKind::NoteOn {
                key: 69,
                velocity: 127,
                channel: 0,
                note_id: 7,
            },
        };
        let off = Event {
            offset: 6,
            kind: EventKind::NoteOff {
                key: 69,
                channel: 0,
                note_id: 7,
            },
        };
        let out = run(&mut sine, &[0.0; 8], &[on, off]);
        assert_eq!(&out[..3], &[0.0; 3]);
        assert_eq!(out[3], TEST_SINE_LEVEL);
        assert!(out[4] > 0.0 && out[4] < TEST_SINE_LEVEL);
        assert_eq!(&out[6..], &[0.0; 2]);
        sine.reset();
        let entries = log.drain();
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries[1],
            Logged::Event {
                frame: 3,
                kind: on.kind
            }
        );
        assert_eq!(entries[3], Logged::Reset { frame: 8 });
        assert_eq!(log.lost(), 0);
    }

    #[test]
    fn gain_follows_its_parameter_and_latency() {
        let mut gain = TestGain::with_latency(0.5, 2);
        assert_eq!(gain.latency(), 2);
        let set = Event {
            offset: 3,
            kind: EventKind::Param {
                id: TEST_GAIN_PARAM,
                value: 2.0,
            },
        };
        let out = run(&mut gain, &[1.0, 2.0, 3.0, 4.0, 5.0], &[set]);
        assert_eq!(out, [0.0, 0.0, 0.5, 4.0, 6.0]);
        gain.reset();
        let out = run(&mut gain, &[1.0, 1.0, 1.0], &[]);
        assert_eq!(out, [0.0, 0.0, 2.0]);
    }
}
