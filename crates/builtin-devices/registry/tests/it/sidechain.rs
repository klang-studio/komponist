//! Presse driven by the pre- and post-fader engine sidechain taps.

use komponist_builtin::Internal;
use komponist_device_core::InternalProcessor;
use komponist_engine::compile::{self, NoEnv};
use komponist_engine::{
    Command, Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_model::edits::{self, ChannelProp, NewDevice, NewTrack, TrackProp};
use komponist_model::params::ParamId;
use komponist_model::{ChannelRef, DeviceRole, PluginFormat, PluginRef, Project, Sidechain, Tap};

use crate::common::{RATE, Rig};

struct Source {
    frame: usize,
    burst: bool,
}

impl Processor for Source {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(&mut self, _: &ProcessContext, _: &[Event], io: &mut StereoIo<'_>) -> ProcessStatus {
        for i in 0..io.frames() {
            let level = if self.burst {
                if self.frame % 12_000 < 2_400 {
                    0.8
                } else {
                    0.0
                }
            } else {
                0.05
            };
            io.output[0][i] = level;
            io.output[1][i] = level;
            self.frame += 1;
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

fn plugin(name: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: format!("test.{name}"),
        name: name.to_owned(),
        vendor: "Test".to_owned(),
        version: "1.0".to_owned(),
        module: Box::default(),
    }
}

fn presse() -> InternalProcessor {
    let kind = Internal::Presse;
    let mut values = kind.defaults();
    for (key, value) in [
        ("threshold", -24.0),
        ("ratio", 6.0),
        ("knee", 0.0),
        ("attack", 0.5),
        ("release", 20.0),
        ("detector", 0.0),
        ("auto_release", 0.0),
    ] {
        values[kind.wire(&ParamId::new(key)).unwrap() as usize] = value;
    }
    let mut p = kind.processor(&values);
    p.activate(RATE, 256).unwrap();
    p
}

/// A muted burst keys a compressor on a quieter constant input. Only the pad reaches the master.
fn ducked_pad(tap: Option<Tap>, fader: f32) -> (f32, f32) {
    let project = Project::new("Kick and pad");
    let add_track = |project: &Project, name| {
        edits::add_track(
            project,
            NewTrack {
                name: None,
                hue: None,
                instrument: Some(plugin(name)),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap()
    };
    let (project, kick) = add_track(&project, "kick");
    let (project, pad) = add_track(&project, "pad");
    let spec = NewDevice {
        role: DeviceRole::Effect,
        plugin: plugin("compressor"),
        state: None,
        sidechain: tap.map(|tap| Sidechain { source: kick, tap }),
    };
    let (project, compressor) =
        edits::insert_device(&project, ChannelRef::Track(pad), 1, spec).unwrap();
    let channel =
        |p: &Project, prop| edits::set_track_prop(p, &[kick], TrackProp::Channel(prop)).unwrap();
    let project = channel(&project, ChannelProp::Mute(true));
    let project = channel(&project, ChannelProp::VolumeDb(fader));
    let kick_device = project.track(kick).unwrap().channel.devices[0].id;
    let mut rig = Rig::new(RATE, 256);
    let program = compile::build(&project, None, &NoEnv);
    for (device, slot) in compile::slot_changes(None, &program).added {
        if device == compressor {
            rig.insert(slot.0, presse());
        } else {
            rig.insert(
                slot.0,
                Source {
                    frame: 0,
                    burst: device == kick_device,
                },
            );
        }
    }
    rig.send(Command::SetProgram(Box::new(program)));
    rig.process(12_000, 256);
    (rig.l[2_300], rig.l[11_900])
}

fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[track_caller]
fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= 1e-5 * expected.abs().max(1.0),
        "{what}: {actual} != {expected}"
    );
}

#[test]
fn presse_ducks_a_pad_by_the_kick_on_its_sidechain() {
    let (hit, quiet) = ducked_pad(Some(Tap::Pre), 0.0);
    assert_close(quiet, 0.05, "recovered between kicks");
    // (−2 dB − (−24 dB)) × (1/6 − 1) = −18.3 dB.
    let ducked = 0.05 * db_to_gain(-18.3);
    assert!(
        (hit - ducked).abs() < 0.2 * ducked,
        "ducked on the kick: {hit} vs {ducked}"
    );
    // Without a source the pad is under the threshold: not touched.
    let (hit, quiet) = ducked_pad(None, 0.0);
    assert_close(hit, 0.05, "no source: no ducking");
    assert_close(quiet, 0.05, "no source: no ducking");
}

#[test]
fn the_pre_tap_ducks_with_the_source_fader_at_minus_infinity_and_the_post_tap_does_not() {
    let (pre, _) = ducked_pad(Some(Tap::Pre), f32::NEG_INFINITY);
    assert!(pre < 0.05 * db_to_gain(-12.0), "pre tap still ducks: {pre}");
    let (post, _) = ducked_pad(Some(Tap::Post), f32::NEG_INFINITY);
    assert_close(post, 0.05, "post tap follows the fader");
}
