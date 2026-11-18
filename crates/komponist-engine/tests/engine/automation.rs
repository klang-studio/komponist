use komponist_engine::automation::{AutomationBoundary, EngineTarget, Parameter, PlainMap};
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::{
    Command, EngineEvent, Event, EventKind, MidiPort, ProcessContext, ProcessStatus, Processor,
    ProcessorError, StereoIo,
};
use komponist_model::automation::{
    AutomationMode, AutomationOwner, AutomationTarget, Point, Points, Shape,
};
use komponist_model::edits::{self, NewTrack, automation::*};
use komponist_model::params::ParamId;
use komponist_model::{Device, DeviceId, PluginFormat, PluginRef, Project, Tick, TickRange};

use crate::common::{RATE, Rig};

struct Parameters(bool);

impl CompileEnv for Parameters {
    fn midi_port(&self, _: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, _: DeviceId) -> u32 {
        0
    }

    fn automation_parameter(&self, _: &Device, param: &ParamId) -> Option<Parameter> {
        Some(Parameter {
            wire_id: param.plugin_id()?,
            base: 0.25,
            map: PlainMap::linear(0.0, 1.0, 0),
        })
        .filter(|_| self.0)
    }
}

/// Constant source scaled by wire parameter zero and a log of the actual event samples.
struct Probe {
    pos: usize,
    gain: f32,
    log: Option<rtrb::Producer<(usize, u32, f64)>>,
}

impl Processor for Probe {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(
        &mut self,
        _: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let mut start = 0;
        for event in events {
            if let EventKind::Param { id, value } = event.kind {
                let end = event.offset as usize;
                io.output[0][start..end].fill(self.gain);
                io.output[1][start..end].fill(self.gain);
                if let Some(log) = &mut self.log {
                    log.push((self.pos + end, id, value)).unwrap();
                }
                if id == 0 {
                    self.gain = value as f32;
                }
                start = end;
            }
        }
        io.output[0][start..].fill(self.gain);
        io.output[1][start..].fill(self.gain);
        self.pos += io.frames();
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

type ParamLog = rtrb::Consumer<(usize, u32, f64)>;

fn source() -> (Project, AutomationOwner, DeviceId) {
    add_source(&Project::new("Automation"))
}

fn add_source(project: &Project) -> (Project, AutomationOwner, DeviceId) {
    let (project, track) = edits::add_track(
        project,
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(PluginRef {
                format: PluginFormat::Clap,
                id: "test".into(),
                name: "Test source".into(),
                vendor: String::new(),
                version: String::new(),
                module: Box::default(),
            }),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let device = project.track(track).unwrap().channel.devices[0].id;
    (project, AutomationOwner::Track(track), device)
}

fn rig(project: &Project) -> (Rig, komponist_engine::Program, ParamLog) {
    let program = compile::build(project, None, &Parameters(true));
    let mut rig = Rig::new(RATE, 2048);
    let (log, reader) = rtrb::RingBuffer::new(8192);
    rig.insert(
        program.tracks[0].chain[0].slot.0,
        Probe {
            pos: 0,
            gain: 1.0,
            log: Some(log),
        },
    );
    rig.set_program(&program);
    (rig, program, reader)
}

fn parameter(device: DeviceId, id: u32) -> AutomationTarget {
    AutomationTarget::Param {
        device,
        param: ParamId::plugin(id),
    }
}

fn lane(
    project: &Project,
    owner: AutomationOwner,
    target: AutomationTarget,
    mode: AutomationMode,
    points: Vec<Point>,
) -> (Project, komponist_model::LaneId) {
    let (project, lane) = add_lane(project, owner, target, "Test".into()).unwrap();
    let project = set_lane_points(&project, owner, lane, Points::new(points)).unwrap();
    (set_lane_mode(&project, owner, lane, mode).unwrap(), lane)
}

#[test]
fn fixed_grid_hold_jump_and_master_pan_match_across_blocks_and_offline() {
    let (project, owner, device) = source();
    let (project, _) = lane(
        &project,
        owner,
        parameter(device, 0),
        AutomationMode::Read,
        vec![
            Point {
                tick: Tick::ZERO,
                value: 0.2,
                shape: Shape::Hold,
            },
            Point::linear(Tick(3), 0.8),
            Point::linear(Tick(122), 1.0),
        ],
    );
    let (project, _) = lane(
        &project,
        AutomationOwner::Master,
        AutomationTarget::Pan,
        AutomationMode::Read,
        vec![
            Point::linear(Tick::ZERO, 0.5),
            Point::linear(Tick(122), 1.0),
        ],
    );
    let run = |block, offline| {
        let (mut rig, program, mut log) = rig(&project);
        assert_eq!(program.master.automation.len(), 1);
        rig.send(Command::Play);
        if offline {
            rig.render(3400, block);
        } else {
            rig.process(3400, block);
        }
        let events = core::iter::from_fn(|| log.pop().ok()).collect::<Vec<_>>();
        (rig.l, rig.r, events)
    };
    let expected = run(64, false);
    let jump = &expected.2[1];
    assert_eq!(jump.0, 75); // Tick 3 is not on the 64-frame ramp grid.
    assert!((jump.2 - 0.8).abs() < 1e-6);
    assert!(expected.0[3000] < expected.1[3000]);
    for block in [256, 1000, 2048] {
        assert_eq!(run(block, false), expected);
        assert_eq!(run(block, true), expected);
    }
}

#[test]
fn locate_chases_when_stopped_and_switching_off_restores_base() {
    let (project, owner, device) = source();
    let target = parameter(device, 0);
    let (project, id) = lane(
        &project,
        owner,
        target.clone(),
        AutomationMode::Read,
        vec![
            Point::linear(Tick::ZERO, 0.0),
            Point::linear(Tick(100), 1.0),
        ],
    );
    let (mut rig, program, mut log) = rig(&project);
    rig.send(Command::Locate(Tick(70)));
    rig.process(64, 64);
    assert_eq!(log.pop().unwrap(), (0, 0, f64::from(0.7_f32)));
    assert!(log.pop().is_err());
    let off = set_lane_mode(&project, owner, id, AutomationMode::Off).unwrap();
    rig.set_program(&compile::build(&off, Some(&program), &Parameters(true)));
    rig.process(64, 64);
    assert_eq!(log.pop().unwrap(), (64, 0, 0.25));
    assert_eq!(rig.l[64], 0.25);
    assert!(log.pop().is_err());
}

#[test]
fn loop_wrap_chases_and_restarts_only_a_held_touch() {
    let (mut project, owner, device) = source();
    project.loop_on = true;
    project.loop_range = TickRange::new(Tick(2), Tick(8));
    let target = parameter(device, 0);
    let (project, _) = lane(
        &project,
        owner,
        target.clone(),
        AutomationMode::Touch,
        vec![Point::linear(Tick::ZERO, 0.1), Point::linear(Tick(8), 0.9)],
    );
    let (mut rig, program, _) = rig(&project);
    let target = program.automation_target(owner, &target).unwrap();
    let slot = program.slots.slot(device).unwrap();
    rig.send(Command::Play);
    rig.send(Command::Touch {
        owner,
        target,
        begin: true,
    });
    rig.send(Command::SetParam {
        slot,
        id: 0,
        value: 0.4,
    });
    rig.process(250, 250);
    let events = rig.take_events();
    let boundary = events
        .iter()
        .position(|event| {
            matches!(
                event,
                EngineEvent::AutomationBoundary {
                    tick: Tick(8),
                    kind: AutomationBoundary::LoopWrap
                }
            )
        })
        .unwrap();
    assert!(matches!(
        events[boundary + 1],
        EngineEvent::LoopWrapped { to: Tick(2) }
    ));
    assert!(matches!(
        events[boundary + 2],
        EngineEvent::AutomationTouch {
            begin: true,
            tick: Tick(2),
            ..
        }
    ));
    rig.send(Command::Touch {
        owner,
        target,
        begin: false,
    });
    rig.process(16, 16);
    assert!(rig.events.iter().any(|event| matches!(
        event,
        EngineEvent::AutomationTouch {
            begin: false,
            tick: Tick(4),
            ..
        }
    )));
}

#[test]
fn short_touch_sends_begin_value_then_release_and_returns_to_existing_curve() {
    let (project, owner, device) = source();
    let target = parameter(device, 0);
    let (project, _) = lane(
        &project,
        owner,
        target.clone(),
        AutomationMode::Touch,
        vec![Point::linear(Tick::ZERO, 0.25)],
    );
    let (mut rig, program, _) = rig(&project);
    let target = program.automation_target(owner, &target).unwrap();
    let slot = program.slots.slot(device).unwrap();
    rig.send(Command::Play);
    rig.send(Command::Touch {
        owner,
        target,
        begin: true,
    });
    rig.send(Command::SetParam {
        slot,
        id: 0,
        value: 0.75,
    });
    rig.send(Command::Touch {
        owner,
        target,
        begin: false,
    });
    rig.process(12_500, 1000);
    assert!(matches!(
        &rig.events[..],
        [
            EngineEvent::AutomationTouch {
                tick: Tick::ZERO,
                begin: true,
                ..
            },
            EngineEvent::AutomationValue {
                tick: Tick::ZERO,
                value: 0.75,
                ..
            },
            EngineEvent::AutomationTouch {
                tick: Tick::ZERO,
                begin: false,
                ..
            },
        ]
    ));
    assert_eq!(rig.l[0], 0.75);
    assert!((rig.l[6000] - 0.5).abs() < 0.01);
    assert_eq!(rig.l[12_499], 0.25);
}

#[test]
fn same_project_recompiles_when_parameter_descriptor_appears_or_disappears() {
    let (project, owner, device) = source();
    let target = parameter(device, 0);
    let (project, _) = lane(
        &project,
        owner,
        target.clone(),
        AutomationMode::Read,
        vec![Point::linear(Tick::ZERO, 0.5)],
    );
    let missing = compile::build(&project, None, &Parameters(false));
    assert_eq!(missing.automation_target(owner, &target), None);
    let loaded = compile::build(&project, Some(&missing), &Parameters(true));
    assert!(matches!(
        loaded.automation_target(owner, &target),
        Some(EngineTarget::Slot { .. })
    ));
    let dropped = compile::build(&project, Some(&loaded), &Parameters(false));
    assert_eq!(dropped.automation_target(owner, &target), None);
}

#[test]
fn overflow_retains_the_latest_value_for_each_target() {
    let (mut project, owner, device) = source();
    for id in 0..40 {
        (project, _) = lane(
            &project,
            owner,
            parameter(device, id),
            AutomationMode::Read,
            vec![Point::linear(Tick::ZERO, 0.0), Point::linear(Tick(80), 1.0)],
        );
    }
    let (mut rig, _, mut log) = rig(&project);
    rig.send(Command::Play);
    rig.process(2048, 2048);
    let events = core::iter::from_fn(|| log.pop().ok()).collect::<Vec<_>>();
    assert_eq!(events.len(), komponist_engine::MAX_EVENTS_PER_BLOCK);
    assert!(rig.handle.telemetry().dropped_events() > 0);
    assert!((0..40).all(|id| {
        events
            .iter()
            .any(|&(frame, wire, _)| wire == id && frame == 64 * 31)
    }));
}

/// Time the incremental automation work on the audio thread with 16 × 16 ramping targets.
#[test]
#[ignore = "release-only realtime budget measurement"]
fn ramp_playback_budget() {
    use std::time::{Duration, Instant};

    let mut base = Project::new("Automation performance");
    let devices: Vec<_> = (0..16)
        .map(|_| {
            let (next, owner, device) = add_source(&base);
            base = next;
            (owner, device)
        })
        .collect();
    let mut ramping = base.clone();
    for (owner, device) in devices {
        for wire in 0..16 {
            ramping = lane(
                &ramping,
                owner,
                parameter(device, wire),
                AutomationMode::Read,
                vec![
                    Point::linear(Tick::ZERO, 0.1),
                    Point::linear(Tick(60_000), 0.95),
                ],
            )
            .0;
        }
    }

    let elapsed = |project: &Project| {
        const FRAMES: usize = 256;
        let mut rig = Rig::new(RATE, FRAMES);
        let program = compile::build(project, None, &Parameters(true));
        for track in &program.tracks {
            rig.insert(
                track.chain[0].slot.0,
                Probe {
                    pos: 0,
                    gain: 0.1,
                    log: None,
                },
            );
        }
        rig.set_program(&program);
        rig.send(Command::Play);
        rig.process(FRAMES * 80, FRAMES); // Fault in the preallocated tables first.
        let mut left = [0.0; FRAMES];
        let mut right = [0.0; FRAMES];
        let mut total = Duration::ZERO;
        for _ in 0..1_000 {
            let start = Instant::now();
            assert_no_alloc::assert_no_alloc(|| {
                rig.engine.process(
                    &mut left,
                    &mut right,
                    &komponist_engine::LiveInput {
                        midi: &[],
                        output_latency: 0,
                    },
                );
            });
            total += start.elapsed();
        }
        total / 1_000
    };
    let base = elapsed(&base);
    let with_automation = elapsed(&ramping);
    let difference = with_automation.saturating_sub(base);
    eprintln!(
        "16 × 16 automation ramps, 256 @ 48 kHz: {base:?} baseline, {with_automation:?} full, {difference:?} automation"
    );
    assert!(difference <= Duration::from_secs_f64(0.02 * 256.0 / 48_000.0));
}
