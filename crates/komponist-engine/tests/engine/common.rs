//! Helpers: programs built by hand (the compiler is WP-24's) and a rig that drives the engine with
//! every `process` / `render` call inside `assert_no_alloc`.

use std::ops::Range;
use std::sync::Arc;

use assert_no_alloc::assert_no_alloc;
use komponist_engine::program::{
    ChainSlot, InputFilter, MasterProgram, SeqNote, SlotMap, TrackProgram,
};
use komponist_engine::testing::{EventLog, Logged};
use komponist_engine::{
    Command, Engine, EngineConfig, EngineEvent, EngineHandle, EventKind, LiveInput, MidiInEvent,
    Processor, Program, SlotId, TrackMeter,
};
use komponist_model::{
    Channel, DeviceId, DeviceRole, Hue, Meter, MidiChannelSel, MidiInputSel, Tempo, TempoMap, Tick,
    TickRange, Track, TrackId, TrackKind,
};

/// Default sample rate of the tests.
pub const RATE: f32 = 48_000.0;

/// One 4/4 bar in ticks.
pub const BAR: i64 = 3840;

/// A sequencer note at full velocity, always playing.
pub fn note(start: i64, length: i64, key: u8) -> SeqNote {
    SeqNote {
        start: Tick(start),
        length: Tick(length),
        key,
        velocity: 127,
        probability: 100,
        channel: 0,
    }
}

/// An instrument device in `slot` (device id = 1000 + slot).
pub fn instrument(slot: u32) -> ChainSlot {
    ChainSlot {
        device: DeviceId(1000 + u64::from(slot)),
        slot: SlotId(slot),
        role: DeviceRole::Instrument,
        bypass: false,
    }
}

/// A MIDI effect device in `slot` (WP-88).
pub fn midi_effect(slot: u32) -> ChainSlot {
    ChainSlot {
        role: DeviceRole::MidiEffect,
        ..instrument(slot)
    }
}

/// An effect device in `slot`.
pub fn effect(slot: u32) -> ChainSlot {
    ChainSlot {
        role: DeviceRole::Effect,
        ..instrument(slot)
    }
}

/// The model track a track program claims to come from.
fn model_track(id: u64) -> Arc<Track> {
    Arc::new(Track {
        id: TrackId(id),
        name: format!("Spur {id}"),
        hue: Hue::RED,
        kind: TrackKind::Midi,
        channel: Channel::default(),
        arm: false,
        monitor: false,
        input: MidiInputSel::All,
        midi_channel: MidiChannelSel::Omni,
        audio: komponist_model::AudioIo::default(),
        height: 56,
        clips: Vec::new(),
        output: komponist_model::TrackOutput::Master,
        sends: Vec::new(),
        folder: false,
        folded: false,
        automation: Default::default(),
        macros: Default::default(),
    })
}

/// Track `id` playing `notes` on the instrument in `slot`, unity gain, centred.
pub fn track(id: u64, slot: u32, notes: Vec<SeqNote>) -> TrackProgram {
    TrackProgram {
        macros: Default::default(),
        automation: Default::default(),
        chain_latency: 0,
        id: TrackId(id),
        source: model_track(id),
        notes: notes.into(),
        chain: vec![instrument(slot)],
        gain: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
        arm: false,
        monitor: false,
        input: InputFilter {
            port: None,
            channel: None,
            port_missing: false,
        },
        pdc_delay: 0,
        meter: Arc::new(TrackMeter::new()),
        audio: None,
    }
}

/// A program at `bpm` in 4/4 with `tracks`; loop over bars 1–4 but off, no metronome, one bar
/// count-in, unity master.
pub fn program(bpm: f64, tracks: Vec<TrackProgram>) -> Program {
    Program {
        tempo_map: TempoMap::constant(Tempo::new(bpm), Meter::FOUR_FOUR),
        loop_range: TickRange::new(Tick(0), Tick(4 * BAR)),
        loop_on: false,
        metronome: false,
        count_in_bars: 1,
        song_end: Tick(0),
        tracks: tracks.into_iter().map(Arc::new).collect(),
        master: MasterProgram {
            automation: Default::default(),
            automation_source: Default::default(),
            chain_latency: 0,
            chain: Vec::new(),
            gain: 1.0,
            pan: 0.0,
            mute: false,
        },
        slots: Arc::new(SlotMap::default()),
        routing: Default::default(),
    }
}

/// The sample of `tick` in `program` at `rate`.
pub fn sample(program: &Program, tick: i64, rate: f32) -> i64 {
    program
        .tempo_map
        .tick_to_sample(Tick(tick), f64::from(rate))
}

/// Drives an engine and collects its output and events.
pub struct Rig {
    pub engine: Box<Engine>,
    pub handle: EngineHandle,
    /// Left and right output of all calls so far.
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// Events received so far, garbage excluded.
    pub events: Vec<EngineEvent>,
    /// Garbage objects received (and dropped on this thread).
    pub garbage: usize,
}

impl Rig {
    pub fn new(sample_rate: f32, max_block: usize) -> Rig {
        Rig::with_config(EngineConfig {
            sample_rate,
            max_block,
            seed: 1,
        })
    }

    pub fn with_config(config: EngineConfig) -> Rig {
        let (engine, handle) = Engine::new(config);
        Rig {
            engine,
            handle,
            l: Vec::new(),
            r: Vec::new(),
            events: Vec::new(),
            garbage: 0,
        }
    }

    pub fn send(&mut self, command: Command) {
        self.handle.send(command).expect("command queue full");
    }

    pub fn insert(&mut self, slot: u32, processor: impl Processor + 'static) {
        self.send(Command::InsertProcessor {
            slot: SlotId(slot),
            processor: Box::new(processor),
        });
    }

    pub fn set_program(&mut self, program: &Program) {
        self.send(Command::SetProgram(Box::new(program.clone())));
    }

    /// Frames output so far.
    pub fn frames(&self) -> usize {
        self.l.len()
    }

    /// Runs `frames` frames through `Engine::process` in calls of `block`; returns their range in
    /// `l`/`r`.
    pub fn process(&mut self, frames: usize, block: usize) -> Range<usize> {
        let start = self.frames();
        let mut done = 0;
        while done < frames {
            let n = block.min(frames - done);
            self.process_with(n, &[], 0);
            done += n;
        }
        start..self.frames()
    }

    /// One `Engine::process` call of `frames` with live MIDI.
    pub fn process_with(&mut self, frames: usize, midi: &[MidiInEvent], latency: u32) {
        let start = self.frames();
        self.l.resize(start + frames, f32::NAN);
        self.r.resize(start + frames, f32::NAN);
        let (l, r) = (&mut self.l[start..], &mut self.r[start..]);
        let engine = &mut self.engine;
        let input = LiveInput {
            midi,
            output_latency: latency,
        };
        assert_no_alloc(|| engine.process(l, r, &input));
        self.poll();
    }

    /// Runs `frames` frames through `Engine::render` in calls of `block`.
    pub fn render(&mut self, frames: usize, block: usize) -> Range<usize> {
        let start = self.frames();
        self.l.resize(start + frames, f32::NAN);
        self.r.resize(start + frames, f32::NAN);
        let mut done = 0;
        while done < frames {
            let n = block.min(frames - done);
            let (l, r) = (
                &mut self.l[start + done..start + done + n],
                &mut self.r[start + done..start + done + n],
            );
            let engine = &mut self.engine;
            assert_no_alloc(|| {
                let out = engine.render(n);
                for (i, frame) in out.as_chunks::<2>().0.iter().enumerate() {
                    l[i] = frame[0];
                    r[i] = frame[1];
                }
            });
            self.poll();
            done += n;
        }
        start..self.frames()
    }

    /// Takes the engine's events; garbage is counted and dropped here, on the "UI thread".
    pub fn poll(&mut self) {
        while let Some(event) = self.handle.poll() {
            match event {
                EngineEvent::Retired(_) => self.garbage += 1,
                other => self.events.push(other),
            }
        }
    }

    /// Takes the collected events.
    pub fn take_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut self.events)
    }
}

/// Note events of a log: `(frame, "on"/"off", key, note id)`.
pub fn notes(log: &[Logged]) -> Vec<(u64, &'static str, u8, u32)> {
    log.iter()
        .filter_map(|entry| match *entry {
            Logged::Event {
                frame,
                kind: EventKind::NoteOn { key, note_id, .. },
            } => Some((frame, "on", key, note_id)),
            Logged::Event {
                frame,
                kind: EventKind::NoteOff { key, note_id, .. },
            } => Some((frame, "off", key, note_id)),
            _ => None,
        })
        .collect()
}

/// Drains a log.
pub fn drain(log: &mut EventLog) -> Vec<Logged> {
    assert_eq!(log.lost(), 0, "test log overflowed");
    log.drain()
}

/// Indices where a sound starts: a non-zero sample after at least 64 zeros (or after only zeros
/// from the start). Sines pass through exact zeros, so a single zero is no gap.
pub fn onsets(buf: &[f32]) -> Vec<usize> {
    (0..buf.len())
        .filter(|&i| buf[i] != 0.0 && buf[i.saturating_sub(64)..i].iter().all(|&x| x == 0.0))
        .collect()
}
