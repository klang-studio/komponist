//! The routing graph in the engine (WP-102, ARCHITECTURE §6.8.4–6.8.7).

use std::collections::HashMap;

use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::testing::{TestGain, TestSine};
use komponist_engine::{Command, MidiPort, Program, SlotId};
use komponist_model::fixtures::demo_project;
use komponist_model::{DeviceId, DeviceRole, Project, Tick, TrackId};

use crate::common::*;

/// Latencies of the effect devices of a project, as a compile environment.
struct Latencies(HashMap<DeviceId, u32>);

impl CompileEnv for Latencies {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.0.get(&device).copied().unwrap_or(0)
    }
}

/// Compiles `project` with a latency of `(id % 4) × 37` frames on every effect and inserts a
/// `TestSine` per instrument and a `TestGain` (0.8, that latency) per effect.
fn load(rig: &mut Rig, project: &Project) -> Program {
    let mut latencies = HashMap::new();
    let devices = project
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .chain(&project.master.devices);
    for device in devices {
        if device.role == DeviceRole::Effect {
            latencies.insert(device.id, (device.id.0 % 4) as u32 * 37);
        }
    }
    let env = Latencies(latencies);
    let program = compile::build(project, None, &env);
    for (device, slot) in compile::slot_changes(None, &program).added {
        let (_, _, model) = project.find_device(device).unwrap();
        match model.role {
            DeviceRole::Instrument => rig.insert(slot.0, TestSine::new()),
            DeviceRole::Effect => rig.insert(
                slot.0,
                TestGain::with_latency(0.8, env.device_latency(device)),
            ),
            DeviceRole::MidiEffect => {}
        }
    }
    rig.set_program(&program);
    program
}

/// FNV-1a over the bits of both channels.
fn hash(l: &[f32], r: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for x in l.iter().chain(r) {
        for b in x.to_bits().to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The demo project (no buses) with test processors and latencies: played with fast-path solo
/// and mute changes, then rendered offline; `workers` worker threads.
fn demo_render(workers: usize) -> (Vec<f32>, Vec<f32>) {
    let mut rig = Rig::new(RATE, 256);
    rig.engine.set_workers(workers, None);
    let project = demo_project();
    let program = load(&mut rig, &project);
    rig.send(Command::Locate(program.loop_range.start));
    rig.send(Command::Play);
    let ids: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
    let blocks = [256, 256, 100, 512, 37, 2048];
    for call in 0..60 {
        rig.process(blocks[call % blocks.len()], 256);
        match call {
            10 => rig.send(Command::SetTrackSolo {
                track: ids[2],
                solo: true,
            }),
            20 => rig.send(Command::SetTrackMute {
                track: ids[2],
                mute: true,
            }),
            30 => rig.send(Command::SetTrackSolo {
                track: ids[2],
                solo: false,
            }),
            40 => rig.set_program(&program),
            _ => {}
        }
    }
    rig.send(Command::Locate(Tick(program.loop_range.start.0 + BAR)));
    rig.render(48_000, 1000);
    (rig.l, rig.r)
}

/// Golden: a project without buses renders bit-identically to the engine before WP-102 (the
/// hash was taken from a render of the base commit `56a0a27`).
#[test]
fn projects_without_buses_render_as_before() {
    const BEFORE: u64 = 3_480_190_116_315_408_716;
    for workers in [0, 3] {
        let (l, r) = demo_render(workers);
        assert!(l.iter().any(|x| x.abs() > 0.01), "silent");
        assert_eq!(hash(&l, &r), BEFORE, "{workers} workers");
    }
}

#[allow(dead_code)]
fn unused(_: SlotId) {}
