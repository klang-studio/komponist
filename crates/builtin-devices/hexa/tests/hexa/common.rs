//! Helpers: driving a `HexaProcessor` call by call with every `process` and `reset` inside
//! `assert_no_alloc`, and measuring its output.

use assert_no_alloc::assert_no_alloc;
use komponist_engine::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, Processor, StereoIo, TransportInfo,
};
use komponist_hexa::{HexaProcessor, Macro, Patch};
use komponist_model::{Meter, Tick, TickRange};

/// Sample rate of the tests.
pub const RATE: f32 = 48_000.0;

/// Largest call the processors are activated for.
pub const MAX_BLOCK: usize = 2048;

/// Level counted as sound: −100 dBFS, above the oversampling filter's pre-ringing (< 2e-6).
pub const AUDIBLE: f32 = 1e-5;

/// Frames in `seconds` at [`RATE`].
pub fn frames(seconds: f32) -> usize {
    (seconds * RATE) as usize
}

/// A processor for `patch`, activated at [`RATE`].
pub fn activated(patch: Patch) -> HexaProcessor {
    let mut p = HexaProcessor::new(patch);
    p.activate(RATE, MAX_BLOCK).expect("48 kHz is supported");
    p
}

pub fn note_on(key: u8) -> EventKind {
    EventKind::NoteOn {
        key,
        velocity: 100,
        channel: 0,
        note_id: NOTE_ID_NONE,
    }
}

pub fn note_off(key: u8) -> EventKind {
    EventKind::NoteOff {
        key,
        channel: 0,
        note_id: NOTE_ID_NONE,
    }
}

pub fn param(m: Macro, value: f64) -> EventKind {
    EventKind::Param { id: m.id(), value }
}

/// An event the processor ignores (unknown parameter id): it only splits the render, so a
/// reference run can be split at the same frames as the run under test.
pub fn nothing() -> EventKind {
    EventKind::Param {
        id: 999,
        value: 0.0,
    }
}

fn ctx(frames: usize) -> ProcessContext {
    ProcessContext {
        frames: frames as u32,
        sample_rate: RATE,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick::ZERO, Tick::ZERO),
        },
        offline: false,
        deadline: None,
    }
}

/// Stereo output collected over several calls.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Out {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

impl Out {
    pub fn len(&self) -> usize {
        self.l.len()
    }

    /// Largest magnitude of both channels in `range`.
    pub fn peak(&self, range: std::ops::Range<usize>) -> f32 {
        self.l[range.clone()]
            .iter()
            .chain(&self.r[range])
            .fold(0.0, |m, x| m.max(x.abs()))
    }

    /// RMS of both channels in `range`.
    pub fn rms(&self, range: std::ops::Range<usize>) -> f32 {
        let n = 2 * range.len();
        let sum: f64 = self.l[range.clone()]
            .iter()
            .chain(&self.r[range])
            .map(|&x| f64::from(x) * f64::from(x))
            .sum();
        (sum / n as f64).sqrt() as f32
    }

    /// First frame where either channel is louder than `level`.
    pub fn onset(&self, level: f32) -> Option<usize> {
        (0..self.len()).find(|&i| self.l[i].abs() > level || self.r[i].abs() > level)
    }

    /// First frame where either channel is not exactly 0.
    pub fn first_nonzero(&self) -> Option<usize> {
        self.onset(0.0)
    }

    /// First frame where `self` and `other` differ.
    pub fn first_difference(&self, other: &Out) -> Option<usize> {
        (0..self.len().min(other.len()))
            .find(|&i| self.l[i] != other.l[i] || self.r[i] != other.r[i])
    }
}

/// One `process` call of `frames` frames inside `assert_no_alloc`, appended to `out`.
pub fn call(p: &mut HexaProcessor, frames: usize, events: &[Event], out: &mut Out) {
    let input = vec![0.0; frames];
    let start = out.len();
    out.l.resize(start + frames, f32::NAN);
    out.r.resize(start + frames, f32::NAN);
    let (l, r) = (&mut out.l[start..], &mut out.r[start..]);
    let mut io = StereoIo {
        input: [&input, &input],
        output: [l, r],
    };
    let context = ctx(frames);
    assert_no_alloc(|| p.process(&context, events, &mut io));
}

/// Plays `total` frames in calls of `block` frames; `events` are `(absolute frame, event)` in
/// order. Returns the output.
pub fn play(
    p: &mut HexaProcessor,
    total: usize,
    block: usize,
    events: &[(usize, EventKind)],
) -> Out {
    let mut out = Out::default();
    let mut done = 0;
    while done < total {
        let n = block.min(total - done);
        let local: Vec<Event> = events
            .iter()
            .filter(|(at, _)| (done..done + n).contains(at))
            .map(|&(at, kind)| Event {
                offset: (at - done) as u32,
                kind,
            })
            .collect();
        call(p, n, &local, &mut out);
        done += n;
    }
    out
}

/// `reset` inside `assert_no_alloc`.
pub fn reset(p: &mut HexaProcessor) {
    assert_no_alloc(|| p.reset());
}
