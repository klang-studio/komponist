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
        self.0.then_some(Parameter {
            wire_id: param.plugin_id()?,
            base: 0.25,
            map: PlainMap::linear(0.0, 1.0, 0),
        })
    }
}

/// Constant source scaled by wire parameter zero and a log of the actual event samples.
struct Probe {
    pos: usize,
    gain: f32,
    log: Option<rtrb::Producer<(usize, u32, f64)>>,
    edits: Option<std::array::IntoIter<komponist_engine::processor::ParameterEdit, 3>>,
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

    fn pop_parameter_edit(&mut self) -> Option<komponist_engine::processor::ParameterEdit> {
        self.edits.as_mut()?.next()
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
            edits: None,
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
    let (mut rig, program, mut log) = rig(&project);
    let target = program.automation_target(owner, &target).unwrap();
    let slot = program.slots.slot(device).unwrap();
    rig.send(Command::Play);
    rig.process(17, 17);
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
    assert_eq!(rig.l[17], 0.75);
    assert!((rig.l[6000] - 0.5).abs() < 0.01);
    assert_eq!(rig.l[12_499], 0.25);
    assert_eq!(
        core::iter::from_fn(|| log.pop().ok()).last(),
        Some((12_017, 0, 0.25))
    );
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
            .any(|&(frame, wire, value)| wire == id && frame == 2000 && value == 1.0)
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
                    edits: None,
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

#[test]
fn plugin_gui_edges_and_value_use_the_engine_clock() {
    use komponist_engine::processor::{ParameterEdit, ParameterEditKind};

    let (project, owner, device) = source();
    let (project, _) = lane(
        &project,
        owner,
        parameter(device, 0),
        AutomationMode::Latch,
        Vec::new(),
    );
    let program = compile::build(&project, None, &Parameters(true));
    let slot = program.slots.slot(device).unwrap();
    let edits = [
        ParameterEdit {
            offset: 25,
            kind: ParameterEditKind::Gesture { id: 0, begin: true },
        },
        ParameterEdit {
            offset: 50,
            kind: ParameterEditKind::Value { id: 0, value: 0.5 },
        },
        ParameterEdit {
            offset: 75,
            kind: ParameterEditKind::Gesture {
                id: 0,
                begin: false,
            },
        },
    ];
    let mut rig = Rig::new(RATE, 1024);
    rig.insert(
        slot.0,
        Probe {
            pos: 0,
            gain: 0.25,
            log: None,
            edits: Some(edits.into_iter()),
        },
    );
    rig.set_program(&program);
    rig.send(Command::Play);
    rig.process(100, 100);
    let events = rig.take_events();
    assert!(matches!(
        &events[0..3],
        [
            EngineEvent::AutomationTouch {
                begin: true,
                tick: Tick(1),
                ..
            },
            EngineEvent::AutomationValue {
                value: 0.5,
                tick: Tick(2),
                ..
            },
            EngineEvent::AutomationTouch {
                begin: false,
                tick: Tick(3),
                ..
            },
        ]
    ));
    let EngineEvent::PluginParams {
        slot: captured_slot,
        events,
    } = &events[3]
    else {
        panic!("expected batched plugin events, got {:?}", events[3]);
    };
    assert_eq!(*captured_slot, slot);
    for (index, event) in events.as_slice().iter().enumerate() {
        assert_eq!(event.tick, Tick((index + 1) as i64));
        assert_eq!(event.kind, edits[index].kind);
    }
    // Latching continues through release and ends at the exact old playhead on locate.
    rig.process(100, 100);
    assert!(matches!(
        &rig.events[..],
        [EngineEvent::AutomationValue {
            value: 0.5,
            tick: Tick(7),
            ..
        }]
    ));
    rig.send(Command::Locate(Tick(20)));
    rig.process(25, 25);
    assert!(rig.events.iter().any(|event| matches!(
        event,
        EngineEvent::AutomationBoundary {
            kind: AutomationBoundary::Locate,
            tick: Tick(8)
        }
    )));
}

#[test]
fn locate_ends_the_pass_even_after_touch_release() {
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
    rig.process(100, 100);
    rig.take_events();
    rig.send(Command::Locate(Tick(20)));
    rig.process(25, 25);
    assert!(matches!(
        &rig.events[..],
        [EngineEvent::AutomationBoundary {
            kind: AutomationBoundary::Locate,
            tick: Tick(4)
        }]
    ));
}

#[test]
fn read_override_does_not_return_on_a_curve_that_visits_the_same_value_again() {
    let (project, owner, device) = source();
    let target = parameter(device, 0);
    let (project, _) = lane(
        &project,
        owner,
        target.clone(),
        AutomationMode::Read,
        vec![
            Point::linear(Tick::ZERO, 0.25),
            Point::linear(Tick(2), 0.25),
            Point::linear(Tick(3), 0.8),
            Point::linear(Tick(4), 0.25),
        ],
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
    rig.process(256, 256);
    assert_eq!(rig.l[0], 0.75); // The curve is still on the release value.
    assert_eq!(rig.l[180], 0.25); // The curve changed, then returned within this callback.
    assert!(rig.events.is_empty());
}

#[test]
fn a_sub_grid_peak_and_stepped_threshold_reach_the_processor() {
    struct Stepped;
    impl CompileEnv for Stepped {
        fn midi_port(&self, _: &str) -> Option<MidiPort> {
            None
        }
        fn device_latency(&self, _: DeviceId) -> u32 {
            0
        }
        fn automation_parameter(&self, _: &Device, param: &ParamId) -> Option<Parameter> {
            let wire_id = param.plugin_id()?;
            Some(Parameter {
                wire_id,
                base: 0.0,
                map: PlainMap::linear(0.0, 1.0, u32::from(wire_id == 1)),
            })
        }
    }

    let (project, owner, device) = source();
    let (project, _) = lane(
        &project,
        owner,
        parameter(device, 0),
        AutomationMode::Read,
        vec![
            Point::linear(Tick(0), 0.0),
            Point::linear(Tick(1), 1.0),
            Point::linear(Tick(2), 0.0),
        ],
    );
    let (project, _) = lane(
        &project,
        owner,
        parameter(device, 1),
        AutomationMode::Read,
        vec![
            Point::linear(Tick(0), 0.499999),
            Point::linear(Tick(1), 0.500001),
        ],
    );
    let (mut rig, initial, mut log) = rig(&project);
    rig.set_program(&compile::build(&project, Some(&initial), &Stepped));
    rig.send(Command::Play);
    rig.process(128, 128);
    assert_eq!(&rig.l[24..26], &[0.0, 1.0]);
    assert_eq!(&rig.l[49..51], &[1.0, 0.0]);
    assert!(core::iter::from_fn(|| log.pop().ok()).any(|event| event == (25, 1, 1.0)));
}

#[test]
fn a_master_pan_hold_jumps_at_its_sample_and_reopening_still_fades_in() {
    let (project, _, _) = source();
    let (project, _) = lane(
        &project,
        AutomationOwner::Master,
        AutomationTarget::Pan,
        AutomationMode::Read,
        vec![
            Point {
                tick: Tick(0),
                value: 0.5,
                shape: Shape::Hold,
            },
            Point::linear(Tick(3), 1.0),
        ],
    );
    let (mut rig, p, _) = rig(&project);
    rig.send(Command::Play);
    rig.process(128, 128);
    assert_eq!(&rig.l[64..75], &[1.0; 11]);
    assert_eq!(&rig.l[75..80], &[0.0; 5]);
    let mut muted = project.clone();
    std::sync::Arc::make_mut(&mut muted.master).mute = true;
    let paused = compile::build(&muted, Some(&p), &Parameters(true));
    rig.set_program(&paused);
    rig.process(4, 4);
    assert_eq!(rig.r[131], 0.0);
    rig.set_program(&p);
    rig.process(8, 4);
    assert_eq!(
        &rig.r[132..140],
        &[0.25, 0.5, 0.75, 1.0, 1.0, 1.0, 1.0, 1.0]
    );
}

#[test]
fn deleting_a_lane_restores_base_once_and_off_respects_newer_manual_edits() {
    let (project, owner, device) = source();
    let target = parameter(device, 0);
    let (project, id) = lane(
        &project,
        owner,
        target,
        AutomationMode::Read,
        vec![Point::linear(Tick(0), 0.8)],
    );
    let (mut rig, p, mut log) = rig(&project);
    rig.process(64, 64);
    assert_eq!(rig.l[63], 0.8);
    let deleted = remove_lane(&project, owner, id).unwrap();
    rig.set_program(&compile::build(&deleted, Some(&p), &Parameters(true)));
    rig.process(64, 64);
    assert_eq!(rig.l[64], 0.25);
    assert_eq!(
        core::iter::from_fn(|| log.pop().ok()).last(),
        Some((64, 0, 0.25))
    );
    rig.process(64, 64);
    assert!(log.pop().is_err());

    rig.set_program(&p);
    rig.process(64, 64);
    let off = set_lane_mode(&project, owner, id, AutomationMode::Off).unwrap();
    rig.set_program(&compile::build(&off, Some(&p), &Parameters(true)));
    rig.send(Command::SetParam {
        slot: p.slots.slot(device).unwrap(),
        id: 0,
        value: 0.75,
    });
    rig.process(64, 64);
    assert_eq!(rig.l[256], 0.75);
    rig.send(Command::Play);
    rig.process(64, 64);
    assert_eq!(rig.l[320], 0.75);
}

#[test]
fn a_fresh_touch_without_movement_does_not_restore_its_previous_value() {
    let (project, _, _) = source();
    let owner = AutomationOwner::Master;
    let target = AutomationTarget::Pan;
    let (project, _) = lane(
        &project,
        owner,
        target.clone(),
        AutomationMode::Touch,
        vec![Point::linear(Tick(0), 0.75)],
    );
    let (mut rig, program, _) = rig(&project);
    let touch = |begin| Command::Touch {
        owner,
        target: EngineTarget::Pan,
        begin,
    };
    rig.send(Command::Play);
    rig.send(touch(true));
    rig.send(Command::SetMasterPan(-0.5));
    rig.process(128, 128);
    assert_eq!(rig.r[127], 0.5);
    assert_eq!(program.automation_override(owner, &target), Some(0.25));
    rig.send(touch(false));
    rig.process(12_256, 512);
    assert_eq!(rig.r[12_383], 1.0);
    assert_eq!(program.automation_override(owner, &target), None);
    rig.take_events();

    rig.send(touch(true));
    rig.process(128, 128);
    rig.send(touch(false));
    rig.process(128, 128);
    assert_eq!(&rig.r[12_384..], &[1.0; 256]);
    assert!(
        !rig.events
            .iter()
            .any(|event| matches!(event, EngineEvent::AutomationValue { .. }))
    );
}

#[test]
fn flatten_scope_removes_master_and_target_strip_lanes_only() {
    use komponist_engine::export::TrackExport;
    use komponist_model::edits::{NewClip, NewClipContent};

    let (project, owner, device) = source();
    let AutomationOwner::Track(track) = owner else {
        unreachable!()
    };
    let (mut project, _) = edits::add_clip(
        &project,
        track,
        NewClip {
            start: Tick::ZERO,
            length: Tick(960),
            name: "Flatten".into(),
            content: NewClipContent::Midi {
                notes: Vec::new(),
                take_name: String::new(),
            },
        },
    )
    .unwrap();
    let parameter = parameter(device, 0);
    for (owner, target) in [
        (owner, AutomationTarget::Volume),
        (owner, AutomationTarget::Pan),
        (owner, parameter.clone()),
        (AutomationOwner::Master, AutomationTarget::Volume),
    ] {
        (project, _) = lane(
            &project,
            owner,
            target,
            AutomationMode::Read,
            vec![Point::linear(Tick(0), 0.75)],
        );
    }
    let export = TrackExport::new(&project, track).unwrap();
    assert!(!project.master_automation.lanes.is_empty());
    assert_eq!(project.track(track).unwrap().automation.lanes.len(), 3);
    assert!(export.project().master_automation.lanes.is_empty());
    let lanes = &export.project().track(track).unwrap().automation.lanes;
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].target, parameter);
}

#[test]
fn plugin_touch_return_starts_after_the_reporting_block() {
    use komponist_engine::processor::{ParameterEdit, ParameterEditKind};

    let (project, owner, device) = source();
    let (project, _) = lane(
        &project,
        owner,
        parameter(device, 0),
        AutomationMode::Touch,
        vec![Point::linear(Tick::ZERO, 0.25)],
    );
    let program = compile::build(&project, None, &Parameters(true));
    let slot = program.slots.slot(device).unwrap();
    let edits = [
        ParameterEdit {
            offset: 25,
            kind: ParameterEditKind::Gesture { id: 0, begin: true },
        },
        ParameterEdit {
            offset: 50,
            kind: ParameterEditKind::Value { id: 0, value: 0.75 },
        },
        ParameterEdit {
            offset: 75,
            kind: ParameterEditKind::Gesture {
                id: 0,
                begin: false,
            },
        },
    ];
    let (tx, mut log) = rtrb::RingBuffer::new(300);
    let mut rig = Rig::new(RATE, 2048);
    rig.insert(
        slot.0,
        Probe {
            pos: 0,
            gain: 0.25,
            log: Some(tx),
            edits: Some(edits.into_iter()),
        },
    );
    rig.set_program(&program);
    rig.send(Command::Play);
    rig.process(2048, 2048);
    assert_eq!(log.pop().unwrap(), (0, 0, 0.25));
    assert!(rig.events.iter().any(|event| matches!(
        event,
        EngineEvent::AutomationTouch {
            begin: false,
            tick: Tick(3),
            ..
        }
    )));
    rig.process(128, 128);
    let (sample, id, value) = log.pop().unwrap();
    assert_eq!((sample, id), (2048 + 64, 0));
    assert!((value - 0.747333).abs() < 1e-6, "{value}");
}

#[test]
fn bypass_drains_pending_plugin_edits_at_the_engine_boundary() {
    use komponist_engine::processor::{ParameterEdit, ParameterEditKind};

    let (mut project, owner, device) = source();
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut project.tracks[0])
            .channel
            .devices[0],
    )
    .bypass = true;
    let (project, _) = lane(
        &project,
        owner,
        parameter(device, 0),
        AutomationMode::Touch,
        vec![Point::linear(Tick::ZERO, 0.25)],
    );
    let program = compile::build(&project, None, &Parameters(true));
    let slot = program.slots.slot(device).unwrap();
    let edits = [
        ParameterEdit {
            offset: 25,
            kind: ParameterEditKind::Gesture { id: 0, begin: true },
        },
        ParameterEdit {
            offset: 50,
            kind: ParameterEditKind::Value { id: 0, value: 0.75 },
        },
        ParameterEdit {
            offset: 75,
            kind: ParameterEditKind::Gesture {
                id: 0,
                begin: false,
            },
        },
    ];
    let mut rig = Rig::new(RATE, 128);
    rig.insert(
        slot.0,
        Probe {
            pos: 0,
            gain: 0.25,
            log: None,
            edits: Some(edits.into_iter()),
        },
    );
    rig.set_program(&program);
    rig.send(Command::Play);
    rig.process(128, 128);
    let batch = rig
        .events
        .iter()
        .find_map(|event| match event {
            EngineEvent::PluginParams { events, .. } => Some(events.as_slice()),
            _ => None,
        })
        .unwrap();
    assert_eq!(batch.len(), 3);
    for (received, edit) in batch.iter().zip(edits) {
        assert_eq!(received.tick, Tick::ZERO);
        assert_eq!(received.kind, edit.kind);
    }
}

#[test]
fn a_reloaded_wire_rebuilds_the_curve_when_its_model_target_changes() {
    struct Binding(u32);
    impl CompileEnv for Binding {
        fn midi_port(&self, _: &str) -> Option<MidiPort> {
            None
        }
        fn device_latency(&self, _: DeviceId) -> u32 {
            0
        }
        fn automation_parameter(&self, _: &Device, param: &ParamId) -> Option<Parameter> {
            (param.plugin_id()? == self.0).then_some(Parameter {
                wire_id: 0,
                map: PlainMap::linear(0.0, 1.0, 0),
                base: 0.25,
            })
        }
    }
    let (project, owner, device) = source();
    let (project, _) = lane(
        &project,
        owner,
        parameter(device, 0),
        AutomationMode::Read,
        vec![Point::linear(Tick::ZERO, 0.3)],
    );
    let (project, _) = lane(
        &project,
        owner,
        parameter(device, 1),
        AutomationMode::Read,
        vec![Point::linear(Tick::ZERO, 0.8)],
    );
    let a = compile::build(&project, None, &Binding(0));
    let b = compile::build(&project, Some(&a), &Binding(1));
    assert_eq!(b.tracks[0].automation[0].model, parameter(device, 1));
    assert_eq!(b.tracks[0].automation[0].points[0].value, 0.8);
}

#[test]
fn midi_effect_chases_only_when_its_instrument_can_consume_stage_output() {
    use komponist_engine::testing::{TestMidiFx, TestSine};
    use komponist_model::edits::{NewClip, NewClipContent, NewDevice, NewNote};
    use komponist_model::{ChannelRef, DeviceRole};
    use std::sync::Arc;

    let (project, owner, instrument) = source();
    let AutomationOwner::Track(track) = owner else {
        unreachable!()
    };
    let effect = NewDevice {
        role: DeviceRole::MidiEffect,
        plugin: project.tracks[0].channel.devices[0].plugin.clone(),
        state: None,
        sidechain: None,
    };
    let (project, effect) =
        edits::insert_device(&project, ChannelRef::Track(track), 0, effect).unwrap();
    let (project, _) = edits::add_clip(
        &project,
        track,
        NewClip {
            start: Tick::ZERO,
            length: Tick(100),
            name: "Stage test".into(),
            content: NewClipContent::Midi {
                notes: vec![NewNote {
                    start: Tick(10),
                    length: Tick(10),
                    pitch: 60,
                    velocity: 100,
                }],
                take_name: String::new(),
            },
        },
    )
    .unwrap();
    let (project, _) = lane(
        &project,
        owner,
        parameter(effect, 0),
        AutomationMode::Read,
        vec![Point::linear(Tick::ZERO, 1.0)],
    );
    let mut bypassed = project.clone();
    let track = Arc::make_mut(&mut bypassed.tracks[0]);
    Arc::make_mut(&mut track.channel.devices[1]).bypass = true;
    let muted = compile::build(&bypassed, None, &Parameters(true));
    let unbypassed = compile::build(&project, Some(&muted), &Parameters(true));
    let (sine, mut log) = TestSine::logged(32);
    let mut rig = Rig::new(RATE, 256);
    rig.insert(muted.slots.slot(instrument).unwrap().0, sine);
    let mut midi_effect = TestMidiFx::transposing(0);
    let mut parameter_log = midi_effect.log_parameters(8);
    rig.insert(muted.slots.slot(effect).unwrap().0, midi_effect);
    rig.set_program(&muted);
    rig.send(Command::Play);
    rig.process(128, 128);
    assert!(parameter_log.pop().is_err());
    rig.set_program(&unbypassed);
    rig.process(256, 256);
    assert_eq!(parameter_log.pop().unwrap(), (0, 1.0));
    assert!(parameter_log.pop().is_err());
    assert_eq!(crate::common::notes(&log.drain())[0].2, 61);
}

#[test]
fn sub_grid_mixer_peak_reaches_the_ordinary_point_in_every_block_partition() {
    let (project, _, _) = source();
    let (project, _) = lane(
        &project,
        AutomationOwner::Master,
        AutomationTarget::Pan,
        AutomationMode::Read,
        vec![
            Point::linear(Tick::ZERO, 0.5),
            Point::linear(Tick(1), 0.9),
            Point::linear(Tick(2), 0.5),
        ],
    );
    let run = |block| {
        let (mut rig, _, _) = rig(&project);
        rig.send(Command::Play);
        rig.process(64, block);
        rig.l
    };
    let samples = run(64);
    assert_eq!(samples[0], 1.0);
    assert!((samples[25] - 0.2).abs() < 1e-6);
    assert_eq!(samples[50], 1.0);
    assert_eq!(samples, run(17));
}

#[test]
fn pending_base_restore_on_a_bypassed_plugin_yields_only_to_a_new_gui_value() {
    use komponist_engine::processor::{ParameterEdit, ParameterEditKind};
    use std::sync::Arc;

    for has_value in [false, true] {
        let (mut project, owner, device) = source();
        Arc::make_mut(&mut Arc::make_mut(&mut project.tracks[0]).channel.devices[0]).bypass = true;
        let (project, lane) = lane(
            &project,
            owner,
            parameter(device, 0),
            AutomationMode::Read,
            vec![Point::linear(Tick::ZERO, 0.8)],
        );
        let (mut rig, program, _) = rig(&project);
        let slot = program.slots.slot(device).unwrap();
        let mut deleted = remove_lane(&project, owner, lane).unwrap();
        let bypassed = compile::build(&deleted, Some(&program), &Parameters(true));
        let (tx, mut values) = rtrb::RingBuffer::new(8);
        let begin = ParameterEditKind::Gesture { id: 0, begin: true };
        let end = ParameterEditKind::Gesture {
            id: 0,
            begin: false,
        };
        let middle = if has_value {
            ParameterEditKind::Value { id: 0, value: 0.75 }
        } else {
            end
        };
        let edits = [begin, middle, end].map(|kind| ParameterEdit { offset: 0, kind });
        rig.insert(
            slot.0,
            Probe {
                pos: 0,
                gain: 0.75,
                log: Some(tx),
                edits: Some(edits.into_iter()),
            },
        );
        rig.set_program(&bypassed);
        rig.process(64, 64);
        assert!(values.pop().is_err());
        Arc::make_mut(&mut Arc::make_mut(&mut deleted.tracks[0]).channel.devices[0]).bypass = false;
        let awake = compile::build(&deleted, Some(&bypassed), &Parameters(true));
        rig.set_program(&awake);
        rig.process(64, 64);
        if has_value {
            assert_eq!(rig.l[64], 0.75);
            assert!(values.pop().is_err());
        } else {
            assert_eq!(rig.l[64], 0.25);
            assert_eq!(values.pop().unwrap(), (0, 0, 0.25));
        }
    }
}
