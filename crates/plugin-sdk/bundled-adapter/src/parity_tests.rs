//! The same note and MIDI sequence reaches the `Dsp` identically through every host of a
//! built-in device: the in-app `InternalProcessor` (the reference), the adapter's `Instrument`
//! and the sampler CLAP (#223).

use std::sync::{Arc, Mutex};

use komponist_device_core::{
    CatalogSpec, DeviceNote, Dsp, InternalProcessor, Spec, SpecCache, dispatch_note, no_presets,
};
use komponist_engine::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, Processor, StereoIo, TransportInfo,
};
use komponist_model::params::layout::DeviceLayout;
use komponist_model::{DeviceRole, Meter, Tick, TickRange};
use nice_plug::midi::{Channel, Key, NoteEvent, VoiceID};

use crate::instrument::apply_note;
use crate::to_device_note;

static SPEC: Spec = Spec {
    catalog: CatalogSpec {
        id: "parity-probe",
        name: "Probe",
        name_in: |_| "Probe",
        description_in: None,
        role: DeviceRole::Instrument,
        features: &[],
        vendor: "komponist",
        sidechain: false,
    },
    params: |_| Vec::new(),
    layout: |_| DeviceLayout::knob_grid([], 1),
    presets: no_presets,
    cache: SpecCache::new(),
};

type Log = Arc<Mutex<Vec<String>>>;

/// Records every note-related call.
struct Probe(Log);

impl Probe {
    fn log(&self, call: String) {
        self.0.lock().unwrap().push(call);
    }
}

impl Dsp for Probe {
    fn activate(&mut self, _: f32) {}
    fn set_param(&mut self, _: usize, _: f64) {}
    fn process(&mut self, _: [&[f32]; 2], _: [&mut [f32]; 2]) {}
    fn reset(&mut self) {}
    fn is_instrument(&self) -> bool {
        true
    }
    fn note_on_with_id(&mut self, key: u8, velocity: u8, id: Option<u32>, channel: Option<u8>) {
        self.log(format!("on {key} {velocity} {id:?} {channel:?}"));
    }
    fn note_off_with_id(&mut self, key: u8, id: Option<u32>, channel: Option<u8>) {
        self.log(format!("off {key} {id:?} {channel:?}"));
    }
    fn choke_note(&mut self, key: Option<u8>, id: Option<u32>, channel: Option<u8>) {
        self.log(format!("choke {key:?} {id:?} {channel:?}"));
    }
    fn all_notes_off(&mut self) {
        self.log("all_notes_off".into());
    }
    fn pitch_bend(&mut self, value: f32) {
        self.log(format!("bend {value}"));
    }
    fn mod_wheel(&mut self, value: f32) {
        self.log(format!("mod {value}"));
    }
    fn pressure(&mut self, value: f32) {
        self.log(format!("pressure {value}"));
    }
}

/// One thing the host does, in each host's own event format.
struct Step {
    engine: EventKind,
    clap: NoteEvent<()>,
}

fn on(key: u8, velocity: u8, id: u32) -> Step {
    Step {
        engine: EventKind::NoteOn {
            channel: 2,
            key,
            note_id: id,
            velocity,
        },
        clap: NoteEvent::NoteOn {
            timing: 0,
            voice_id: VoiceID::ID(id as i32),
            channel: Channel::Number(2),
            key: Key::Number(key),
            velocity: f32::from(velocity) / 127.0,
        },
    }
}

fn off(key: u8, id: u32) -> Step {
    Step {
        engine: EventKind::NoteOff {
            channel: 2,
            key,
            note_id: id,
        },
        clap: NoteEvent::NoteOff {
            timing: 0,
            voice_id: VoiceID::ID(id as i32),
            channel: Channel::Number(2),
            key: Key::Number(key),
            velocity: 0.0,
        },
    }
}

fn cc(cc: u8, value: u8) -> Step {
    Step {
        engine: EventKind::Midi([0xB2, cc, value]),
        clap: NoteEvent::MidiCC {
            timing: 0,
            channel: 2,
            cc,
            value: f32::from(value) / 127.0,
        },
    }
}

fn bend(value: u16) -> Step {
    Step {
        engine: EventKind::Midi([0xE2, (value & 0x7F) as u8, (value >> 7) as u8]),
        clap: NoteEvent::MidiPitchBend {
            timing: 0,
            channel: 2,
            value: f32::from(value) / 16383.0,
        },
    }
}

fn pressure(value: u8) -> Step {
    Step {
        engine: EventKind::Midi([0xD2, value, 0]),
        clap: NoteEvent::MidiChannelPressure {
            timing: 0,
            channel: 2,
            pressure: f32::from(value) / 127.0,
        },
    }
}

fn in_app(steps: &[Step]) -> Vec<String> {
    let log = Log::default();
    let mut processor = InternalProcessor::from_dsp(&SPEC, &[], Box::new(Probe(log.clone())));
    processor.activate(48_000.0, 64).unwrap();
    let events: Vec<Event> = steps
        .iter()
        .map(|step| Event {
            offset: 0,
            kind: step.engine,
        })
        .collect();
    let context = ProcessContext {
        frames: 64,
        sample_rate: 48_000.0,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick::ZERO, Tick::ZERO),
        },
        offline: true,
        deadline: None,
    };
    let (input, mut left, mut right) = ([0.0; 64], [0.0; 64], [0.0; 64]);
    processor.process(
        &context,
        &events,
        &mut StereoIo {
            input: [&input, &input],
            output: [&mut left, &mut right],
        },
    );
    log.lock().unwrap().clone()
}

fn adapter_instrument(steps: &[Step]) -> Vec<String> {
    let log = Log::default();
    let mut dsp = Probe(log.clone());
    let mut held = [None; 128];
    for step in steps {
        apply_note(&mut dsp, &mut held, step.clap);
    }
    log.lock().unwrap().clone()
}

fn sampler_clap(steps: &[Step]) -> Vec<String> {
    let log = Log::default();
    let mut dsp = Probe(log.clone());
    for step in steps {
        if let Some(note) = to_device_note(step.clap) {
            dispatch_note(&mut dsp, note);
        }
    }
    log.lock().unwrap().clone()
}

fn assert_parity(name: &str, steps: &[Step]) {
    let reference = in_app(steps);
    assert_eq!(adapter_instrument(steps), reference, "{name}: Instrument");
    assert_eq!(sampler_clap(steps), reference, "{name}: Sampled");
}

#[test]
fn notes_with_velocity_id_and_channel() {
    assert_parity(
        "notes",
        &[
            on(60, 100, 7),
            on(64, 1, 8),
            on(67, 127, 9),
            off(60, 7),
            off(67, 9),
        ],
    );
}

#[test]
fn all_sound_off_and_all_notes_off_end_every_note_through_the_device() {
    for number in [120, 123] {
        let steps = [on(60, 90, 1), on(64, 90, 2), cc(number, 0)];
        let reference = in_app(&steps);
        assert_eq!(reference[2], "all_notes_off");
        assert_parity("channel mode", &steps);
    }
}

#[test]
fn pitch_bend_mod_wheel_and_pressure() {
    assert_parity(
        "controllers",
        &[
            bend(0),
            bend(8192),
            bend(16383),
            bend(12000),
            cc(1, 0),
            cc(1, 64),
            cc(1, 127),
            pressure(0),
            pressure(100),
        ],
    );
}

#[test]
fn ignored_controllers_do_nothing() {
    let steps = [cc(7, 100), cc(64, 127), cc(121, 0), cc(124, 0)];
    assert!(in_app(&steps).is_empty());
    assert_parity("ignored", &steps);
}

#[test]
fn a_clap_note_without_an_id_arrives_as_none() {
    let reference = {
        let log = Log::default();
        let mut dsp = Probe(log.clone());
        dispatch_note(
            &mut dsp,
            DeviceNote::On {
                key: 60,
                velocity: 80,
                note_id: None,
                channel: Some(0),
            },
        );
        log.lock().unwrap().clone()
    };
    let engine = Step {
        engine: EventKind::NoteOn {
            channel: 0,
            key: 60,
            note_id: NOTE_ID_NONE,
            velocity: 80,
        },
        clap: NoteEvent::NoteOn {
            timing: 0,
            voice_id: VoiceID::Wildcard,
            channel: Channel::Number(0),
            key: Key::Number(60),
            velocity: 80.0 / 127.0,
        },
    };
    assert_eq!(in_app(std::slice::from_ref(&engine)), reference);
    assert_parity("no id", &[engine]);
}
