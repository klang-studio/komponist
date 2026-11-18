//! hexa inside the real engine: timing through the sequencer, preset switch by replacing the
//! processor, undo and redo as parameters to the running processor, no allocation in
//! `Engine::process`.

use std::sync::Arc;

use assert_no_alloc::assert_no_alloc;
use komponist_builtin::{HexaProcessor, device, preset_patch};
use komponist_engine::program::{
    ChainSlot, InputFilter, MasterProgram, SeqNote, SlotMap, TrackProgram,
};
use komponist_engine::{
    Command, Engine, EngineConfig, EngineEvent, EngineHandle, Garbage, LiveInput, Processor,
    Program, SlotId, TrackMeter,
};
use komponist_model::{
    Channel, DeviceId, DeviceRole, Hue, Meter, MidiChannelSel, MidiInputSel, Tempo, TempoMap, Tick,
    TickRange, Track, TrackId, TrackKind,
};

use komponist_model::params::ParamId;

use crate::common::{AUDIBLE, MAX_BLOCK, Out, RATE, frames};

const BAR: i64 = 3840;

fn note(start: i64, length: i64, key: u8) -> SeqNote {
    SeqNote {
        start: Tick(start),
        length: Tick(length),
        key,
        velocity: 110,
        probability: 100,
        channel: 0,
    }
}

/// One track playing `notes` on the instrument in slot 0, at `bpm`, unity gain, no metronome.
fn program(bpm: f64, notes: Vec<SeqNote>) -> Program {
    let source = Arc::new(Track {
        id: TrackId(1),
        name: "Bass".to_owned(),
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
    });
    let track = TrackProgram {
        id: TrackId(1),
        source,
        notes: notes.into(),
        chain: vec![ChainSlot {
            device: DeviceId(1000),
            slot: SlotId(0),
            role: DeviceRole::Instrument,
            bypass: false,
        }],
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
    };
    Program {
        tempo_map: TempoMap::constant(Tempo::new(bpm), Meter::FOUR_FOUR),
        loop_range: TickRange::new(Tick(0), Tick(4 * BAR)),
        loop_on: false,
        metronome: false,
        count_in_bars: 1,
        song_end: Tick(0),
        tracks: vec![Arc::new(track)],
        master: MasterProgram {
            chain: Vec::new(),
            gain: 1.0,
            pan: 0.0,
            mute: false,
        },
        slots: Arc::new(SlotMap::default()),
        routing: Default::default(),
    }
}

fn sample(program: &Program, tick: i64) -> usize {
    program
        .tempo_map
        .tick_to_sample(Tick(tick), f64::from(RATE)) as usize
}

/// A hexa processor for factory preset `index`, activated as the app does before inserting.
fn hexa(index: usize, max_block: usize) -> Box<dyn Processor> {
    let mut p = HexaProcessor::new(preset_patch(index).unwrap());
    p.activate(RATE, max_block).unwrap();
    Box::new(p)
}

struct Rig {
    engine: Box<Engine>,
    handle: EngineHandle,
    out: Out,
    garbage: usize,
}

impl Rig {
    fn new(max_block: usize) -> Rig {
        let (engine, handle) = Engine::new(EngineConfig {
            sample_rate: RATE,
            max_block,
            seed: 1,
        });
        Rig {
            engine,
            handle,
            out: Out::default(),
            garbage: 0,
        }
    }

    fn send(&mut self, command: Command) {
        self.handle.send(command).expect("command queue has room");
    }

    /// `frames` frames through `Engine::process` (inside `assert_no_alloc`) in calls of `block`.
    fn process(&mut self, frames: usize, block: usize) {
        let mut done = 0;
        while done < frames {
            let n = block.min(frames - done);
            let start = self.out.len();
            self.out.l.resize(start + n, f32::NAN);
            self.out.r.resize(start + n, f32::NAN);
            let (l, r) = (&mut self.out.l[start..], &mut self.out.r[start..]);
            let engine = &mut self.engine;
            let input = LiveInput {
                midi: &[],
                output_latency: 0,
            };
            assert_no_alloc(|| engine.process(l, r, &input));
            while let Some(event) = self.handle.poll() {
                match event {
                    EngineEvent::Retired(Garbage::Processor(..)) => self.garbage += 1,
                    EngineEvent::ProcessorFailed { .. } => panic!("hexa failed"),
                    _ => {}
                }
            }
            done += n;
        }
    }
}

#[test]
fn sequenced_notes_sound_on_their_samples() {
    // Ticks that land between samples; far enough apart that each note has died away before
    // the next one.
    let ticks = [0_i64, BAR + 50, 2 * BAR + 959, 3 * BAR + 7];
    for (bpm, max_block, block) in [(124.0, 256, 256), (97.5, 64, 173), (240.0, MAX_BLOCK, 4096)] {
        let notes = ticks.iter().map(|&t| note(t, 240, 48)).collect();
        let p = program(bpm, notes);
        let mut rig = Rig::new(max_block);
        rig.send(Command::InsertProcessor {
            slot: SlotId(0),
            processor: hexa(35, max_block),
        });
        rig.send(Command::SetProgram(Box::new(p.clone())));
        rig.send(Command::Play);
        // One note at a time, each starting after the previous one has died away.
        for (i, &t) in ticks.iter().enumerate() {
            let at = sample(&p, t);
            let end = ticks
                .get(i + 1)
                .map_or(at + 20_000, |&next| sample(&p, next));
            rig.process(end - rig.out.len(), block);
            let window = Out {
                l: rig.out.l[at.saturating_sub(64)..end].to_vec(),
                r: rig.out.r[at.saturating_sub(64)..end].to_vec(),
            };
            // Silence before the note, its first (filter) sample on its own sample.
            let base = at.saturating_sub(64);
            assert_eq!(
                window.first_nonzero().map(|i| base + i),
                Some(at),
                "{bpm} bpm, tick {t}"
            );
            let onset = base + window.onset(AUDIBLE).expect("note sounds");
            assert!(
                onset.abs_diff(at + 17) <= 1,
                "{bpm} bpm, tick {t}: onset {onset}, expected {}",
                at + 17
            );
        }
    }
}

#[test]
fn replacing_the_processor_switches_the_preset() {
    let p = program(120.0, vec![note(0, 480, 60), note(BAR, 480, 60)]);
    let second = sample(&p, BAR);
    // The same program once with E-Piano throughout and once switching to FM Bass before the
    // second note.
    let mut piano = Rig::new(512);
    piano.send(Command::InsertProcessor {
        slot: SlotId(0),
        processor: hexa(1, 512),
    });
    piano.send(Command::SetProgram(Box::new(p.clone())));
    piano.send(Command::Play);
    piano.process(second + 24_000, 512);

    let mut switched = Rig::new(512);
    switched.send(Command::InsertProcessor {
        slot: SlotId(0),
        processor: hexa(1, 512),
    });
    switched.send(Command::SetProgram(Box::new(p.clone())));
    switched.send(Command::Play);
    switched.process(second - 4096, 512);
    switched.send(Command::InsertProcessor {
        slot: SlotId(0),
        processor: hexa(35, 512),
    });
    switched.process(second + 24_000 - switched.out.len(), 512);

    // The old processor came back to this thread.
    assert_eq!(switched.garbage, 1);
    // Identical (silent) until the second note, which is FM Bass.
    assert_eq!(piano.out.first_difference(&switched.out), Some(second));
    let range = second..second + 24_000;
    assert!(switched.out.peak(range.clone()) > 0.05);
    assert_ne!(&switched.out.l[range.clone()], &piano.out.l[range]);
}

/// Sends `changes` (from `device::param_changes`) to slot 0, as the app does on undo and redo.
fn send_changes(rig: &mut Rig, changes: &[(u32, f64)]) {
    for &(id, value) in changes {
        rig.send(Command::SetParam {
            slot: SlotId(0),
            id,
            value,
        });
    }
}

#[test]
fn undo_and_redo_as_parameters_keep_a_held_note_sounding() {
    // #19: a knob turn, its undo and its redo reach the running processor as parameters, so a
    // note held across all three sounds on (a fresh processor would be silent until the next
    // note) and follows each patch.
    let original = preset_patch(0).unwrap().sanitized();
    let volume = device::param(&ParamId::new("volume")).unwrap().field;
    let mut turned = original;
    volume.set(&mut turned, volume.get(&original) * 0.5);
    let undo = device::param_changes(&turned, &original).expect("a knob turn is parameters");
    let redo = device::param_changes(&original, &turned).expect("a knob turn is parameters");
    assert_eq!(undo.len(), 1);
    assert_eq!(redo.len(), 1);

    // One note over all four phases; each phase is 0.5 s.
    let p = program(120.0, vec![note(0, 4 * BAR, 60)]);
    let phase = frames(0.5);
    let run = |changes: &[&[(u32, f64)]]| {
        let mut rig = Rig::new(512);
        rig.send(Command::InsertProcessor {
            slot: SlotId(0),
            processor: hexa(0, 512),
        });
        rig.send(Command::SetProgram(Box::new(p.clone())));
        rig.send(Command::Play);
        rig.process(phase, 512);
        for change in changes {
            send_changes(&mut rig, change);
            rig.process(phase, 512);
        }
        assert_eq!(rig.garbage, 0, "the processor stays the same");
        rig.out
    };
    let reference = run(&[&[], &[], &[]]);
    let changed = run(&[&redo, &undo, &redo]);

    // The level of each phase after its glide (hexa glides the volume over 10 ms).
    let ratio = |n: usize| {
        let range = n * phase + frames(0.05)..(n + 1) * phase;
        let (a, b) = (changed.rms(range.clone()), reference.rms(range));
        assert!(b > 1e-3, "the reference note sounds in phase {n}: {b}");
        a / b
    };
    assert!(
        (ratio(0) - 1.0).abs() < 1e-6,
        "before the turn: {}",
        ratio(0)
    );
    assert!((0.4..0.6).contains(&ratio(1)), "turned: {}", ratio(1));
    assert!((0.95..1.05).contains(&ratio(2)), "undo: {}", ratio(2));
    assert!((0.4..0.6).contains(&ratio(3)), "redo: {}", ratio(3));
    // No gap at the changes: every 10 ms of the changed run sound.
    for start in (0..4 * phase).step_by(frames(0.01)) {
        let end = (start + frames(0.01)).min(4 * phase);
        if start >= frames(0.05) {
            assert!(changed.peak(start..end) > 1e-3, "silence at frame {start}");
        }
    }
}
